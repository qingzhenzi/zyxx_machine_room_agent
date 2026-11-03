//! 客户端错误类型。

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("网络/传输错误: {0}")]
    Transport(String),

    #[error("注册链接无效: {0}")]
    InvalidLink(String),

    #[error("token 无效或已过期")]
    TokenInvalid,

    #[error("该节点槽位已被占用（可能已有存活证书）")]
    SlotConflict,

    #[error("挑战值已过期，请重新发起注册")]
    ChallengeExpired,

    #[error("挑战值已被使用，请重新发起注册")]
    ChallengeConsumed,

    #[error("挑战值与会话不匹配")]
    ChallengeMismatch,

    #[error("持有证明被服务端拒绝")]
    PopRejected,

    #[error("服务端证书指纹不符，疑似中间人攻击（已中止）")]
    FingerprintMismatch,

    #[error("注册单待管理员审批，enroll_id={0}")]
    PendingApproval(String),

    #[error("注册单被管理员拒绝")]
    ApprovalRejected,

    #[error("证书已被下载，不可重复获取")]
    AlreadyDownloaded,

    #[error("用户拒绝信任服务端证书")]
    TrustRejected,

    #[error("服务端返回未预期状态: {0}")]
    Unexpected(String),

    #[error("密码学错误: {0}")]
    Crypto(String),

    #[error("本地存储错误: {0}")]
    Storage(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
