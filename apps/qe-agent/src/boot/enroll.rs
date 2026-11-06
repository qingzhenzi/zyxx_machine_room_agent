//! 阶段 5：按配置注册到控制服务器。
//!
//! 语义（与需求一致）：
//! * **配置不足**（无注册链接/凭据）→ `skipped`，状态里标记 `config_sufficient=false`，
//!   不执行后续依赖注册的步骤，但其余初始化照常。
//! * **注册失败** → `failed`，同样不阻塞其余阶段。
//! * 已有有效证书 → 直接 `ok`（复用既有凭据，不重复注册）。

use crate::boot::state::SharedState;
use qe_device_pki_client::creds;

/// 执行注册阶段（同步包装；内部走 async）。
pub fn run(state: &SharedState, data_dir: &std::path::Path, slot: Option<&str>) {
    state.begin(crate::boot::Phase::Enroll);

    let slot_name = slot.unwrap_or("default");
    let cred_dir = creds::Credentials::dir_for(slot_name).unwrap_or_default();

    // 已有有效凭据 → 直接通过。
    if creds::Credentials::exists(&cred_dir) {
        state.set_config_sufficient(true);
        state.finish(
            crate::boot::Phase::Enroll,
            "ok",
            "已有设备证书，跳过注册",
            None,
        );
        return;
    }

    // 找注册链接。
    let link = std::env::var("QE_ENROLL_LINK").ok().or_else(|| {
        std::fs::read_to_string(data_dir.join("enroll-link"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    });

    let Some(link) = link else {
        state.set_config_sufficient(false);
        state.finish(
            crate::boot::Phase::Enroll,
            "skipped",
            "配置信息不足：无证书也无注册链接，等待 provisioning",
            None,
        );
        return;
    };

    state.set_config_sufficient(true);

    // 有链接 → 尝试注册（阻塞 boot 线程可以接受：注册最多等 300s）。
    let target = match qe_device_pki_client::enroll::EnrollTarget::parse(&link) {
        Ok(target) => target,
        Err(error) => {
            state.finish(
                crate::boot::Phase::Enroll,
                "failed",
                format!("注册链接无效: {error}"),
                None,
            );
            return;
        }
    };

    let opts = qe_device_pki_client::enroll::EnrollOptions {
        approval_wait: Some(std::time::Duration::from_secs(300)),
        trust_confirmed: true,
        data_dir: Some(data_dir.to_path_buf()),
    };

    let result = crate::block_on(qe_device_pki_client::enroll::enroll(&target, &opts));
    match result {
        Ok(outcome) => {
            state.finish(
                crate::boot::Phase::Enroll,
                "ok",
                match &outcome {
                    qe_device_pki_client::enroll::EnrollOutcome::Issued(_) => {
                        "注册完成，证书已签发".to_owned()
                    }
                    qe_device_pki_client::enroll::EnrollOutcome::PendingApproval { enroll_id } => {
                        format!("注册待审批（enroll_id={enroll_id}）")
                    }
                },
                None,
            );
        }
        Err(error) => {
            state.finish(
                crate::boot::Phase::Enroll,
                "failed",
                format!("注册失败（不阻塞初始化）: {error}"),
                None,
            );
        }
    }
}
