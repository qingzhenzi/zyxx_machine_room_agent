//! 设备 PKI 客户端：注册、TOFU/pin 信任、mTLS 防抖通讯。
//!
//! 协议契约见仓库根 `docs/device-pki-protocol.md`。
//!
//! 这是**临时验证用 crate**：注册与通讯逻辑验证通过后，可原样移植进真实客户端项目。

pub mod channel;
pub mod command;
pub mod creds;
pub mod downlink;
pub mod enroll;
pub mod error;
pub mod p2p;
pub mod transcript;
pub mod transport;
pub mod trust;

pub use error::{Error, Result};
