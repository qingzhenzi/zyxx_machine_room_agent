use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("非法端口: {0}")]
    InvalidPort(u16),

    #[error("非法来源地址/CIDR: {0}")]
    InvalidCidr(String),
}
