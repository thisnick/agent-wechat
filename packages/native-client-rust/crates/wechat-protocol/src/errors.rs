//! Typed failure reasons carried inside `anyhow::Error`. Messages stay the same
//! as before these types existed; the types let callers branch without parsing.
use anyhow::Result;

/// The response boundary that reported a non-zero service code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Service {
    /// Outer response envelope of an authenticated or prelogin request.
    NativeEnvelope,
    /// QR challenge creation.
    QrLogin,
    /// Polling an issued login challenge.
    LoginPoll,
    /// Confirmation push for a saved device.
    SavedDeviceLogin,
    /// Credential exchange that completes a login.
    AccountAuthentication,
    /// Session renewal with saved credentials.
    AutomaticAuthentication,
    /// Persistent-connection heartbeat.
    Heartbeat,
    /// Message sync.
    Sync,
    /// Contact lookup.
    Contact,
    /// Contact catalog sync.
    ContactSync,
    /// Initial contact catalog.
    ContactBootstrap,
    /// Group member roster.
    Room,
    /// Group catalog page.
    GroupMetadata,
    /// Avatar lookup.
    Avatar,
    /// Voice chunk download.
    Voice,
    /// File upload reservation.
    FileUploadReservation,
    /// CDN credential bootstrap.
    CdnBootstrap,
    /// CDN download.
    CdnDownload,
    /// CDN upload.
    CdnUpload,
    /// Large-file upload.
    LargeFileUpload,
}

impl Service {
    /// Label used at the start of the error message.
    pub fn label(self) -> &'static str {
        match self {
            Self::NativeEnvelope => "Native envelope",
            Self::QrLogin => "QR service",
            Self::LoginPoll => "Login poll",
            Self::SavedDeviceLogin => "Saved-device login",
            Self::AccountAuthentication => "Account authentication",
            Self::AutomaticAuthentication => "Automatic authentication",
            Self::Heartbeat => "Heartbeat",
            Self::Sync => "Sync service",
            Self::Contact => "Contact service",
            Self::ContactSync => "Contact sync service",
            Self::ContactBootstrap => "Contact bootstrap",
            Self::Room => "Room service",
            Self::GroupMetadata => "Group metadata service",
            Self::Avatar => "Avatar service",
            Self::Voice => "Voice service",
            Self::FileUploadReservation => "File upload reservation",
            Self::CdnBootstrap => "CDN bootstrap",
            Self::CdnDownload => "CDN download",
            Self::CdnUpload => "CDN upload",
            Self::LargeFileUpload => "Large-file upload",
        }
    }
}

/// A verified response carried a non-zero service code. The request reached the
/// service, so this error never shows that an operation was not submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRejected {
    /// Response boundary that reported the code.
    pub service: Service,
    /// Signed code returned by that boundary.
    pub code: i32,
}

impl ServiceRejected {
    /// Code the session boundaries return when they no longer accept the
    /// account's credentials.
    pub const SESSION_REJECTED: i32 = -13;

    /// Build the error for one boundary and code.
    pub fn new(service: Service, code: i32) -> Self {
        Self { service, code }
    }

    /// Accept code zero and reject every other code.
    pub(crate) fn check(service: Service, code: i32) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(Self { service, code }.into())
        }
    }

    /// As [`Self::check`] for codes parsed as 64-bit text. A code outside the
    /// 32-bit range keeps the same message without the type.
    pub(crate) fn check_wide(service: Service, code: i64) -> Result<()> {
        match i32::try_from(code) {
            Ok(code) => Self::check(service, code),
            Err(_) => Err(anyhow::anyhow!("{} returned {code}", service.label())),
        }
    }

    /// Find this error anywhere in an error and its causes.
    pub fn find(error: &anyhow::Error) -> Option<&Self> {
        find(error)
    }

    /// True when a session boundary returned [`Self::SESSION_REJECTED`]. The
    /// account needs a new login or renewal; repeating the request cannot help.
    /// The same code from another boundary, such as the CDN, is not included.
    pub fn is_session_rejected(&self) -> bool {
        self.code == Self::SESSION_REJECTED
            && matches!(
                self.service,
                Service::NativeEnvelope
                    | Service::Heartbeat
                    | Service::Sync
                    | Service::Contact
                    | Service::ContactSync
                    | Service::SavedDeviceLogin
                    | Service::AccountAuthentication
                    | Service::AutomaticAuthentication
            )
    }
}

impl std::fmt::Display for ServiceRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} returned {}", self.service.label(), self.code)
    }
}

impl std::error::Error for ServiceRejected {}

/// True when the error shows that the service no longer accepts the account's
/// session: a [`ServiceRejected`] for which
/// [`ServiceRejected::is_session_rejected`] holds, or an
/// [`OpenImServiceError`](crate::OpenImServiceError) with the same code.
pub fn is_session_rejected(error: &anyhow::Error) -> bool {
    ServiceRejected::find(error).is_some_and(ServiceRejected::is_session_rejected)
        || find::<crate::OpenImServiceError>(error)
            .is_some_and(|failure| failure.service_code == ServiceRejected::SESSION_REJECTED)
}

/// Why the account owner ended a request or a notification wait. This says
/// nothing about whether bytes were sent; check
/// [`NativeRequestNotSubmitted`](crate::NativeRequestNotSubmitted) for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AccountInterrupted {
    /// The account was stopped, shut down or reserved for logout. A background
    /// loop should end quietly.
    Stopped,
    /// The credentials were moved out with `quiesce_and_export`. This client
    /// can no longer be used.
    Transferred,
    /// A renewal accepted newer credentials. Open a new notification
    /// connection, or repeat a request that was not submitted.
    CredentialsSuperseded,
}

impl AccountInterrupted {
    /// Find this reason anywhere in an error and its causes.
    pub fn find(error: &anyhow::Error) -> Option<Self> {
        find(error).copied()
    }

    /// The typed reason with the given message shown in front of it.
    pub(crate) fn because(self, message: &'static str) -> anyhow::Error {
        anyhow::Error::new(self).context(message)
    }
}

impl std::fmt::Display for AccountInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stopped => "Account stopped",
            Self::Transferred => "Account state transferred",
            Self::CredentialsSuperseded => "Account credentials superseded",
        })
    }
}

impl std::error::Error for AccountInterrupted {}

fn find<T: std::error::Error + Send + Sync + 'static>(error: &anyhow::Error) -> Option<&T> {
    error
        .downcast_ref::<T>()
        .or_else(|| error.chain().find_map(|cause| cause.downcast_ref::<T>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_rejection_keeps_its_message_and_is_found_under_context() {
        let error = ServiceRejected::check(Service::Heartbeat, -13)
            .unwrap_err()
            .context("request failed")
            .context(crate::NativeRequestNotSubmitted);
        assert_eq!(
            ServiceRejected::find(&error),
            Some(&ServiceRejected::new(Service::Heartbeat, -13))
        );
        assert_eq!(
            error.root_cause().to_string(),
            "Heartbeat returned -13",
            "the message matches the earlier untyped error"
        );
        assert!(ServiceRejected::check(Service::Heartbeat, 0).is_ok());
    }

    #[test]
    fn session_rejection_depends_on_the_boundary_and_the_exact_code() {
        for service in [
            Service::NativeEnvelope,
            Service::Heartbeat,
            Service::Sync,
            Service::Contact,
            Service::ContactSync,
            Service::SavedDeviceLogin,
            Service::AccountAuthentication,
            Service::AutomaticAuthentication,
        ] {
            assert!(is_session_rejected(
                &ServiceRejected::new(service, -13).into()
            ));
            assert!(!is_session_rejected(
                &ServiceRejected::new(service, -130).into()
            ));
        }
        for service in [Service::CdnBootstrap, Service::Voice, Service::LoginPoll] {
            assert!(!is_session_rejected(
                &ServiceRejected::new(service, -13).into()
            ));
        }
        assert!(is_session_rejected(
            &anyhow::Error::new(crate::OpenImServiceError { service_code: -13 }).context("lookup")
        ));
        assert!(!is_session_rejected(&anyhow::anyhow!(
            "Heartbeat returned -13"
        )));
    }

    #[test]
    fn interruption_reason_is_found_behind_its_message() {
        let error = AccountInterrupted::Stopped
            .because("Account stopped during request")
            .context(crate::NativeRequestNotSubmitted);
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::Stopped)
        );
        assert!(error.is::<crate::NativeRequestNotSubmitted>());
        assert_eq!(AccountInterrupted::find(&anyhow::anyhow!("other")), None);
    }
}
