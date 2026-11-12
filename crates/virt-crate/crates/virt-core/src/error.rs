//! Errors shared by all virt backends.

use thiserror::Error;

/// Unified error type for hypervisor operations.
#[derive(Debug, Error)]
pub enum VirtError {
    /// The requested hypervisor backend is not available on this host.
    #[error("hypervisor backend unavailable: {0}")]
    BackendUnavailable(String),

    /// No domain (VM) exists with the given name or id.
    #[error("domain not found: {0}")]
    DomainNotFound(String),

    /// A domain with that name is already registered.
    #[error("domain already exists: {0}")]
    AlreadyExists(String),

    /// The operation is not supported by this backend.
    #[error("unsupported operation on this backend: {0}")]
    Unsupported(&'static str),

    /// Invalid configuration or argument.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// The vCPU is in the wrong state for the requested KVM run.
    #[error("vCPU error: exit reason {reason}: {message}")]
    VcpuExit { reason: String, message: String },

    /// Underlying system/IO error (ioctl, libc, socket...).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Backend-specific error preserved as a string.
    #[error("{backend} error: {message}")]
    Backend {
        backend: &'static str,
        message: String,
    },
}
