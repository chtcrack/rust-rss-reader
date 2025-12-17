// 加密工具模块
// 实现API密钥的加密和解密功能
// 使用AES-256-GCM加密算法，确保数据的机密性、完整性和认证性

use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use aes_gcm::aead::{Aead, AeadCore, Error as AeadError};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use data_encoding::BASE64;
use rand_core::{OsRng, RngCore};

/// 加密相关错误类型
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// AES加密错误
    #[error("AES加密错误: {0}")]
    AesError(String),
    
    /// 编码错误
    #[error("编码错误: {0}")]
    EncodingError(String),
    
    /// 文件I/O错误
    #[error("文件I/O错误: {0}")]
    IoError(#[from] std::io::Error),
    
    /// 密钥长度错误
    #[error("密钥长度错误")]
    InvalidKeyLength,
    
    /// 其他错误
    #[error("其他加密错误: {0}")]
    #[allow(dead_code)]
    Other(String),
}

// 实现From trait，将AeadError转换为CryptoError
impl From<AeadError> for CryptoError {
    fn from(error: AeadError) -> Self {
        CryptoError::AesError(error.to_string())
    }
}

/// 加密工具结构体
pub struct CryptoManager {
    encryption_key: [u8; 32], // AES-256-GCM密钥
}

impl CryptoManager {
    /// 创建新的加密管理器实例
    /// 自动生成或从安全存储加载加密密钥
    pub fn new() -> Result<Self, CryptoError> {
        let encryption_key = Self::get_or_generate_encryption_key()?;
        
        Ok(Self {
            encryption_key,
        })
    }
    
    /// 从安全存储获取或生成加密密钥
    fn get_or_generate_encryption_key() -> Result<[u8; 32], CryptoError> {
        // 获取密钥存储路径
        let key_path = Self::encryption_key_path()?;
        
        // 检查密钥文件是否存在
        if key_path.exists() {
            // 从文件读取密钥
            let key_bytes = fs::read(key_path)?;
            
            if key_bytes.len() != 32 {
                return Err(CryptoError::EncodingError("密钥文件内容无效，长度不正确".to_string()));
            }
            
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            Ok(key)
        } else {
            // 生成新的随机密钥
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            
            // 保存密钥到文件
            fs::write(key_path, key)?;
            
            Ok(key)
        }
    }
    
    /// 获取加密密钥存储路径
    fn encryption_key_path() -> Result<PathBuf, std::io::Error> {
        let mut path = if cfg!(target_os = "windows") {
            dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."))
        } else {
            dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
        };
        
        path.push("rust_rss_reader");
        
        // 确保目录存在
        fs::create_dir_all(&path)?;
        
        path.push("encryption_key.dat");
        Ok(path)
    }
    
    /// 加密字符串（用于API密钥）
    /// 返回格式：base64(nonce) + "." + base64(ciphertext) + "." + base64(tag)
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        // 创建AES-256-GCM密码实例
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|_| CryptoError::InvalidKeyLength)?;
        
        // 生成随机nonce（12字节，GCM推荐大小）
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        
        // 加密数据
        let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes())?;
        
        // 提取tag（最后16字节）
        let tag = &ciphertext[ciphertext.len() - 16..];
        
        // 编码为Base64格式
        let nonce_b64 = BASE64.encode(nonce.as_ref());
        let ciphertext_b64 = BASE64.encode(&ciphertext[..ciphertext.len() - 16]);
        let tag_b64 = BASE64.encode(tag);
        
        // 组合成最终的加密字符串
        Ok(format!("{}.{}.{}", nonce_b64, ciphertext_b64, tag_b64))
    }
    
    /// 解密字符串（用于API密钥）
    pub fn decrypt(&self, encrypted_str: &str) -> Result<String, CryptoError> {
        // 分割加密字符串
        let parts: Vec<&str> = encrypted_str.split('.').collect();
        if parts.len() != 3 {
            return Err(CryptoError::EncodingError("无效的加密字符串格式".to_string()));
        }
        
        let (nonce_b64, ciphertext_b64, tag_b64) = (parts[0], parts[1], parts[2]);
        
        // 解码Base64数据
        let nonce = BASE64.decode(nonce_b64.as_bytes())
            .map_err(|_| CryptoError::EncodingError("无效的nonce编码".to_string()))?;
        
        let ciphertext = BASE64.decode(ciphertext_b64.as_bytes())
            .map_err(|_| CryptoError::EncodingError("无效的密文编码".to_string()))?;
        
        let tag = BASE64.decode(tag_b64.as_bytes())
            .map_err(|_| CryptoError::EncodingError("无效的标签编码".to_string()))?;
        
        // 验证nonce长度
        if nonce.len() != 12 {
            return Err(CryptoError::EncodingError(format!(
                "无效的nonce长度，预期12字节，实际{}字节", 
                nonce.len()
            )));
        }
        
        // 验证标签长度
        if tag.len() != 16 {
            return Err(CryptoError::EncodingError(format!(
                "无效的标签长度，预期16字节，实际{}字节", 
                tag.len()
            )));
        }
        
        // 组合密文和标签
        let mut combined = Vec::with_capacity(ciphertext.len() + tag.len());
        combined.extend_from_slice(&ciphertext);
        combined.extend_from_slice(&tag);
        
        // 创建AES-256-GCM密码实例
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|_| CryptoError::InvalidKeyLength)?;
        
        // 解密数据
        let nonce = Nonce::from_slice(&nonce);
        let plaintext = cipher.decrypt(nonce, combined.as_ref())?;
        
        // 转换为字符串
        String::from_utf8(plaintext)
            .map_err(|e| CryptoError::EncodingError(format!("无效的UTF-8数据: {}", e)))
    }
    
    /// 检查字符串是否已加密
    pub fn is_encrypted(&self, data: &str) -> bool {
        // 检查是否符合加密字符串格式：base64.nonce.base64.ciphertext.base64.tag
        let parts: Vec<&str> = data.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        
        // 尝试解码每个部分，验证是否为有效的Base64编码
        for part in parts {
            if BASE64.decode(part.as_bytes()).is_err() {
                return false;
            }
        }
        
        true
    }
}

/// 测试加密和解密功能
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_encrypt_decrypt() {
        // 创建加密管理器
        let crypto_manager = CryptoManager::new().expect("无法创建加密管理器");
        
        // 测试数据
        let test_data = "test_api_key_123456";
        
        // 加密数据
        let encrypted = crypto_manager.encrypt(test_data).expect("加密数据失败");
        println!("加密后: {}", encrypted);
        
        // 检查是否识别为已加密
        assert!(crypto_manager.is_encrypted(&encrypted));
        
        // 解密数据
        let decrypted = crypto_manager.decrypt(&encrypted).expect("解密数据失败");
        println!("解密后: {}", decrypted);
        
        // 验证解密结果
        assert_eq!(test_data, decrypted);
    }
    
    #[test]
    fn test_is_encrypted() {
        // 创建加密管理器
        let crypto_manager = CryptoManager::new().expect("无法创建加密管理器");
        
        // 测试明文
        let plaintext = "test_api_key_123456";
        assert!(!crypto_manager.is_encrypted(plaintext));
        
        // 测试加密数据
        let encrypted = crypto_manager.encrypt(plaintext).expect("加密数据失败");
        assert!(crypto_manager.is_encrypted(&encrypted));
        
        // 测试无效格式
        let invalid_format = "invalid.format";
        assert!(!crypto_manager.is_encrypted(invalid_format));
        
        let invalid_base64 = "invalid_base64_string.another_invalid_string.yet_another";
        assert!(!crypto_manager.is_encrypted(invalid_base64));
    }
}
