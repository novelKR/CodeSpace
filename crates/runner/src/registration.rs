//! The execution owner's DevGuard registration (CSRG-U2).
//!
//! An [`OwnerRegistration`] lives in the process that owns CodeSpace's executions: the gateway
//! beside its `InProcessRunner`, or the UDS worker beside its own. Each [`report`] runs one
//! bounded registration session of that process through `codespace-devguard`, which registers
//! the session's peer, so the gateway can never register the worker's identity, nor the worker
//! the gateway's. It admits and launches nothing, and no execution path waits for it.
//!
//! Sessions run one at a time: a call that arrives while one runs waits for the next, so every
//! report comes from a session that started after the call arrived.
//!
//! [`report`]: OwnerRegistration::report

use std::sync::Arc;
use std::time::Instant;

use codespace_devguard as devguard;
use codespace_domain::{
    ResourceAuthorityCapability, ResourceAuthorityErrorCode, ResourceAuthorityInfo,
    ResourceAuthorityProvider, ResourceAuthorityReport, ResourceAuthorityRole,
    ResourceAuthorityState, ResourceParticipation, ResourceRegistrationInfo,
    ResourceRegistrationState,
};
use tokio::sync::Mutex;

pub use codespace_devguard::{handoff, Owner, OwnerCredential, OwnerSettings};
pub use codespace_domain::ResourceOwner;

/// The registration of this process as CodeSpace's execution owner.
pub struct OwnerRegistration {
    owner: Arc<devguard::Owner>,
    kind: ResourceOwner,
    last: Mutex<Option<(Instant, ResourceAuthorityInfo)>>,
}

impl OwnerRegistration {
    pub fn new(owner: devguard::Owner, kind: ResourceOwner) -> Arc<Self> {
        Arc::new(Self {
            owner: Arc::new(owner),
            kind,
            last: Mutex::new(None),
        })
    }

    /// The instance identity every session of this owner registers.
    pub fn instance_id(&self) -> &str {
        self.owner.instance_id()
    }

    /// The report of a registration session that started after this call arrived.
    pub async fn report(&self) -> ResourceAuthorityInfo {
        let arrived = Instant::now();
        let mut last = self.last.lock().await;
        if let Some((_, info)) = last.as_ref().filter(|(started, _)| *started >= arrived) {
            return info.clone();
        }
        let started = Instant::now();
        let owner = self.owner.clone();
        // A session that cannot finish is reported as unavailable, never as an error.
        let info = match tokio::task::spawn_blocking(move || owner.register()).await {
            Ok(registration) => registration_info(registration, self.kind),
            Err(_) => not_registered(self.kind, ResourceRegistrationState::Unavailable),
        };
        *last = Some((started, info.clone()));
        info
    }
}

/// A status probe's report, with status participation (CSRG-U1).
pub fn status_info(status: devguard::Status) -> ResourceAuthorityInfo {
    authority_info(status, ResourceParticipation::Status, None)
}

/// A registration session's report: the authority's status from that session and the owner's
/// registration.
pub fn registration_info(
    registration: devguard::Registration,
    owner: ResourceOwner,
) -> ResourceAuthorityInfo {
    let registered = ResourceRegistrationInfo {
        owner,
        state: registration_state(registration.state),
        error_code: registration.error_code.map(error_code),
        pid: registration.pid,
    };
    authority_info(
        registration.status,
        ResourceParticipation::Registration,
        Some(registered),
    )
}

/// A session that could not run to an outcome, as an unavailable authority.
fn not_registered(owner: ResourceOwner, state: ResourceRegistrationState) -> ResourceAuthorityInfo {
    ResourceAuthorityInfo {
        provider: ResourceAuthorityProvider::Devguard,
        participation: ResourceParticipation::Registration,
        governs_execution: false,
        state: ResourceAuthorityState::Unavailable,
        error_code: None,
        report: None,
        registration: Some(ResourceRegistrationInfo {
            owner,
            state,
            error_code: None,
            pid: None,
        }),
    }
}

fn authority_info(
    status: devguard::Status,
    participation: ResourceParticipation,
    registration: Option<ResourceRegistrationInfo>,
) -> ResourceAuthorityInfo {
    ResourceAuthorityInfo {
        provider: ResourceAuthorityProvider::Devguard,
        participation,
        // Neither status nor registration admits or launches anything.
        governs_execution: false,
        state: match status.state {
            devguard::State::Available => ResourceAuthorityState::Available,
            devguard::State::Unavailable => ResourceAuthorityState::Unavailable,
            devguard::State::UntrustedAuthority => ResourceAuthorityState::UntrustedAuthority,
            devguard::State::Incompatible => ResourceAuthorityState::Incompatible,
            devguard::State::CredentialRefused => ResourceAuthorityState::CredentialRefused,
            devguard::State::CredentialUnavailable => ResourceAuthorityState::CredentialUnavailable,
        },
        error_code: status.error_code.map(error_code),
        report: status.report.map(|report| ResourceAuthorityReport {
            protocol: report.protocol,
            capabilities: report.capabilities.into_iter().map(capability).collect(),
            role: role(report.role),
            storage_validated: report.storage_validated,
            registration_ready: report.registration_ready,
            execution_ready: report.execution_ready,
        }),
        registration,
    }
}

fn registration_state(state: devguard::RegistrationState) -> ResourceRegistrationState {
    match state {
        devguard::RegistrationState::Registered => ResourceRegistrationState::Registered,
        devguard::RegistrationState::Unavailable => ResourceRegistrationState::Unavailable,
        devguard::RegistrationState::UntrustedAuthority => {
            ResourceRegistrationState::UntrustedAuthority
        }
        devguard::RegistrationState::Incompatible => ResourceRegistrationState::Incompatible,
        devguard::RegistrationState::CredentialUnavailable => {
            ResourceRegistrationState::CredentialUnavailable
        }
        devguard::RegistrationState::CredentialRefused => {
            ResourceRegistrationState::CredentialRefused
        }
        devguard::RegistrationState::RoleMismatch => ResourceRegistrationState::RoleMismatch,
        devguard::RegistrationState::NotReady => ResourceRegistrationState::NotReady,
        devguard::RegistrationState::Refused => ResourceRegistrationState::Refused,
        devguard::RegistrationState::OwnerMismatch => ResourceRegistrationState::OwnerMismatch,
    }
}

fn error_code(code: devguard::ErrorCode) -> ResourceAuthorityErrorCode {
    match code {
        devguard::ErrorCode::Unauthorized => ResourceAuthorityErrorCode::Unauthorized,
        devguard::ErrorCode::InvalidRequest => ResourceAuthorityErrorCode::InvalidRequest,
        devguard::ErrorCode::AttemptConflict => ResourceAuthorityErrorCode::AttemptConflict,
        devguard::ErrorCode::ResourceUnavailable => ResourceAuthorityErrorCode::ResourceUnavailable,
        devguard::ErrorCode::ResourceControlUnavailable => {
            ResourceAuthorityErrorCode::ResourceControlUnavailable
        }
        devguard::ErrorCode::ResourcePolicyUnsupported => {
            ResourceAuthorityErrorCode::ResourcePolicyUnsupported
        }
        devguard::ErrorCode::InvalidTransition => ResourceAuthorityErrorCode::InvalidTransition,
        devguard::ErrorCode::NotFound => ResourceAuthorityErrorCode::NotFound,
        devguard::ErrorCode::ReconciliationRequired => {
            ResourceAuthorityErrorCode::ReconciliationRequired
        }
        devguard::ErrorCode::JournalInvalid => ResourceAuthorityErrorCode::JournalInvalid,
    }
}

fn capability(capability: devguard::Capability) -> ResourceAuthorityCapability {
    match capability {
        devguard::Capability::DurableAdmission => ResourceAuthorityCapability::DurableAdmission,
        devguard::Capability::FencedLaunch => ResourceAuthorityCapability::FencedLaunch,
        devguard::Capability::PerResourceEvidence => {
            ResourceAuthorityCapability::PerResourceEvidence
        }
        devguard::Capability::StaticControlReservations => {
            ResourceAuthorityCapability::StaticControlReservations
        }
        devguard::Capability::MacosCooperative => ResourceAuthorityCapability::MacosCooperative,
        devguard::Capability::LinuxCgroupV2 => ResourceAuthorityCapability::LinuxCgroupV2,
        devguard::Capability::ParentLease => ResourceAuthorityCapability::ParentLease,
        devguard::Capability::UpgradeDrain => ResourceAuthorityCapability::UpgradeDrain,
    }
}

fn role(role: devguard::Role) -> ResourceAuthorityRole {
    match role {
        devguard::Role::Workload => ResourceAuthorityRole::Workload,
        devguard::Role::ControlService => ResourceAuthorityRole::ControlService,
        devguard::Role::Administrator => ResourceAuthorityRole::Administrator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [(devguard::RegistrationState, ResourceRegistrationState); 10] = [
        (
            devguard::RegistrationState::Registered,
            ResourceRegistrationState::Registered,
        ),
        (
            devguard::RegistrationState::Unavailable,
            ResourceRegistrationState::Unavailable,
        ),
        (
            devguard::RegistrationState::UntrustedAuthority,
            ResourceRegistrationState::UntrustedAuthority,
        ),
        (
            devguard::RegistrationState::Incompatible,
            ResourceRegistrationState::Incompatible,
        ),
        (
            devguard::RegistrationState::CredentialUnavailable,
            ResourceRegistrationState::CredentialUnavailable,
        ),
        (
            devguard::RegistrationState::CredentialRefused,
            ResourceRegistrationState::CredentialRefused,
        ),
        (
            devguard::RegistrationState::RoleMismatch,
            ResourceRegistrationState::RoleMismatch,
        ),
        (
            devguard::RegistrationState::NotReady,
            ResourceRegistrationState::NotReady,
        ),
        (
            devguard::RegistrationState::Refused,
            ResourceRegistrationState::Refused,
        ),
        (
            devguard::RegistrationState::OwnerMismatch,
            ResourceRegistrationState::OwnerMismatch,
        ),
    ];

    fn available() -> devguard::Status {
        devguard::Status {
            state: devguard::State::Available,
            error_code: None,
            report: Some(devguard::Report {
                protocol: 1,
                capabilities: vec![devguard::Capability::StaticControlReservations],
                role: devguard::Role::ControlService,
                storage_validated: true,
                registration_ready: true,
                execution_ready: true,
            }),
        }
    }

    #[test]
    fn every_registration_state_maps_and_never_governs_execution() {
        for (state, expected) in STATES {
            for owner in [ResourceOwner::InProcess, ResourceOwner::Worker] {
                let info = registration_info(
                    devguard::Registration {
                        status: available(),
                        state,
                        error_code: Some(devguard::ErrorCode::AttemptConflict),
                        pid: Some(7),
                    },
                    owner,
                );
                assert_eq!(info.participation, ResourceParticipation::Registration);
                assert!(!info.governs_execution);
                assert_eq!(info.state, ResourceAuthorityState::Available);
                assert_eq!(
                    info.registration,
                    Some(ResourceRegistrationInfo {
                        owner,
                        state: expected,
                        error_code: Some(ResourceAuthorityErrorCode::AttemptConflict),
                        pid: Some(7),
                    })
                );
                assert_eq!(
                    info.report.unwrap().capabilities,
                    [ResourceAuthorityCapability::StaticControlReservations]
                );
            }
        }
    }

    #[test]
    fn codes_capabilities_and_roles_keep_devguard_wire_names() {
        let wire = |value: serde_json::Value| value.as_str().unwrap().to_owned();
        for code in devguard::ErrorCode::ALL {
            assert_eq!(
                wire(serde_json::to_value(error_code(code)).unwrap()),
                code.wire_name()
            );
        }
        for each in devguard::Capability::ALL {
            assert_eq!(
                wire(serde_json::to_value(capability(each)).unwrap()),
                each.wire_name()
            );
        }
        for each in devguard::Role::ALL {
            assert_eq!(
                wire(serde_json::to_value(role(each)).unwrap()),
                each.wire_name()
            );
        }
    }

    #[test]
    fn a_status_report_has_no_registration() {
        let info = status_info(available());
        assert_eq!(info.participation, ResourceParticipation::Status);
        assert!(!info.governs_execution);
        assert_eq!(info.registration, None);
    }

    #[test]
    fn every_status_state_maps_as_a_status_probe_reports_it() {
        for (state, expected) in [
            (
                devguard::State::Available,
                ResourceAuthorityState::Available,
            ),
            (
                devguard::State::Unavailable,
                ResourceAuthorityState::Unavailable,
            ),
            (
                devguard::State::UntrustedAuthority,
                ResourceAuthorityState::UntrustedAuthority,
            ),
            (
                devguard::State::Incompatible,
                ResourceAuthorityState::Incompatible,
            ),
            (
                devguard::State::CredentialRefused,
                ResourceAuthorityState::CredentialRefused,
            ),
            (
                devguard::State::CredentialUnavailable,
                ResourceAuthorityState::CredentialUnavailable,
            ),
        ] {
            let info = status_info(devguard::Status {
                state,
                error_code: Some(devguard::ErrorCode::Unauthorized),
                report: None,
            });
            assert_eq!(info.state, expected);
            assert_eq!(
                info.error_code,
                Some(ResourceAuthorityErrorCode::Unauthorized)
            );
            assert_eq!(info.provider, ResourceAuthorityProvider::Devguard);
            assert!(!info.governs_execution);
        }
    }
}
