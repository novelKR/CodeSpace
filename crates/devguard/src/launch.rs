//! Managed launch of one admitted attempt by its registered owner (CSRG-U4).
//!
//! The owner commits the attempt's launch once with `BeginLaunch`, in a bounded session like
//! the others (`Register`, then the request). Only the first answer carries the attempt's
//! one-time permit, a [`Permit`] here, which cannot be cloned or printed. A lost answer is never
//! answered by asking again: [`LaunchAnswer::Unknown`] leaves the caller to look the attempt up
//! and settle it.
//!
//! [`Owner::helper`] turns the permit into one invocation of DevGuard's launch helper,
//! `devguard-launch`, through DevGuard's own `helper_command`: the helper's arguments, which
//! carry no secret, and two private descriptors DevGuard creates for it, the permit carrier and
//! the writer of the helper's transcript. Both are close-on-exec in this process. The owner
//! starts the helper as its own direct child through its own spawner, which must let exactly
//! those two descriptors reach the helper ([`HelperInvocation::descriptors`]); DevGuard's
//! `HelperCommand::spawn` is not used, so CodeSpace keeps its pipe and PTY spawners. Once the
//! spawn has returned, [`HelperInvocation::into_transcript`] closes this process's copies of
//! both descriptors and keeps the owner's end of the transcript.
//!
//! The helper presents the grant in a session of its own, from the owner's process as its
//! parent. The authority binds the helper's process group as the attempt's scope, applies and
//! reads back the policy, and authorizes the run; only then does the helper report READY,
//! consume nothing more and exec the executable, which keeps the helper's PID. The transcript
//! ([`Transcript::read`]) tells READY apart from what the executable did, and a failure before
//! READY from one after it.
//!
//! Nothing here keeps DevGuard's messages, and the permit never leaves the invocation: it is
//! written into the carrier and dropped.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use devguard_client::launch::{
    helper_command, HelperCommand, HelperPhase, HelperReport, HelperTicket, LaunchOutcome,
};
use devguard_contract as contract;
use devguard_contract::{AttemptKey, Compatibility, Secret, PROTOCOL_VERSION};

use crate::admission::{Admission, Answer, Phase};
use crate::registration::{Owner, Registration};
use crate::{effective_uid, ErrorCode};

/// The attempt's one-time launch permit. Only the first `BeginLaunch` answer carries it. It
/// cannot be cloned or printed, and building the helper invocation consumes it.
pub struct Permit(Secret);

impl Permit {
    /// A permit from its 64 lowercase hexadecimal digits, as a test authority would grant it.
    /// It authorizes nothing by itself: an authority accepts only the permit of its own grant.
    pub fn from_text(text: &str) -> Option<Self> {
        Secret::new(text.to_owned()).ok().map(Self)
    }
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Permit([REDACTED])")
    }
}

/// What a `BeginLaunch` session established.
#[derive(Debug)]
pub enum LaunchAnswer {
    /// DevGuard committed the launch of this owner's attempt, and this is its only permit.
    Granted {
        attempt: crate::admission::Attempt,
        permit: Permit,
    },
    /// DevGuard answered with this error. Each of its requests is one journal transaction that
    /// commits only before a record is answered, so nothing was committed.
    Refused(ErrorCode),
    /// The session ended before `BeginLaunch` was written: nothing was asked.
    NotSent(Registration),
    /// `BeginLaunch` may have reached DevGuard and no permit came back: a timeout, a lost reply,
    /// an EOF, DevGuard's transport code, an answer for another attempt, owner or meaning, or an
    /// answer without a permit. Whether the launch was committed is not known; asking again
    /// would at best return the committed record without its permit.
    Unknown,
}

/// Launch sessions need what admission needs, and fenced launch.
pub(crate) fn launch_compatibility() -> Compatibility {
    Compatibility {
        minimum_protocol: PROTOCOL_VERSION,
        maximum_protocol: PROTOCOL_VERSION,
        required: BTreeSet::from([
            contract::Capability::StaticControlReservations,
            contract::Capability::DurableAdmission,
            contract::Capability::FencedLaunch,
        ]),
    }
}

impl Owner {
    /// Commit the launch of the admitted attempt, once. A caller never sends it twice for one
    /// attempt: after [`LaunchAnswer::Unknown`] it looks the attempt up instead.
    pub fn begin_launch(&self, admission: &Admission) -> LaunchAnswer {
        self.begin_launch_after(admission, effective_uid(), self.pid(), &|| {})
    }

    /// [`Self::begin_launch`], calling `registered` between the registration and the request.
    pub(crate) fn begin_launch_after(
        &self,
        admission: &Admission,
        authority_uid: u32,
        owner_pid: u32,
        registered: &dyn Fn(),
    ) -> LaunchAnswer {
        let Some(wire) = self.wire(admission) else {
            return LaunchAnswer::Refused(ErrorCode::InvalidRequest);
        };
        let Ok(fingerprint) = wire.fingerprint() else {
            return LaunchAnswer::Refused(ErrorCode::InvalidRequest);
        };
        let (mut client, _) = match self.open_with(authority_uid, launch_compatibility(), owner_pid)
        {
            Ok(opened) => opened,
            Err(registration) => return LaunchAnswer::NotSent(registration),
        };
        registered();
        let grant = match client.begin_launch(wire.key.clone()) {
            Ok(grant) => grant,
            Err(error) => {
                return match crate::admission::answered_error(error.code) {
                    Answer::Refused(code) => LaunchAnswer::Refused(code),
                    _ => LaunchAnswer::Unknown,
                }
            }
        };
        // Only this owner's committed record of this attempt and meaning, with its permit, is a
        // grant. A permit that came with anything else is dropped unused.
        match (
            self.attempt(&grant.attempt, &wire.key, &fingerprint),
            grant.permit,
        ) {
            (Answer::Attempt(attempt), Some(permit)) if attempt.phase == Phase::LaunchCommitted => {
                LaunchAnswer::Granted {
                    attempt,
                    permit: Permit(permit),
                }
            }
            _ => LaunchAnswer::Unknown,
        }
    }

    /// The launch helper's invocation for this owner's grant of `admission`: `helper` runs
    /// `program` with `args` once the authority at this owner's socket authorizes it.
    pub fn helper(
        &self,
        helper: &Path,
        admission: &Admission,
        permit: Permit,
        program: &Path,
        args: &[OsString],
    ) -> Result<HelperInvocation, ErrorCode> {
        let ticket = LaunchTicket {
            endpoint: self.settings().socket.clone(),
            consumer: self.consumer().to_owned(),
            generation: self.generation().to_owned(),
            attempt_id: admission.attempt_id.clone(),
            instance_id: self.instance_id().to_owned(),
        };
        HelperInvocation::new(helper, &ticket, permit, program, args)
    }
}

/// Whose grant a helper presents, and where. None of it is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTicket {
    /// The authority's socket the owner's sessions use.
    pub endpoint: PathBuf,
    pub consumer: String,
    pub generation: String,
    pub attempt_id: String,
    /// The owner's registered instance.
    pub instance_id: String,
}

/// One grant's helper invocation, ready for the owner's own spawner. It owns this process's
/// copies of the permit carrier and the transcript writer, both close-on-exec, until
/// [`Self::into_transcript`]; dropping it closes them too.
pub struct HelperInvocation {
    helper: PathBuf,
    args: Vec<OsString>,
    descriptors: [RawFd; 2],
    /// Holds the two descriptors; never spawned.
    _command: HelperCommand,
    report: HelperReport,
}

impl std::fmt::Debug for HelperInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperInvocation")
            .field("helper", &self.helper)
            .field("args", &self.args)
            .field("descriptors", &self.descriptors)
            .finish_non_exhaustive()
    }
}

impl HelperInvocation {
    /// Build the invocation for `ticket` through DevGuard's `helper_command`, which creates the
    /// two descriptors above the standard three, close-on-exec, under its process-wide guard.
    pub fn new(
        helper: &Path,
        ticket: &LaunchTicket,
        permit: Permit,
        program: &Path,
        args: &[OsString],
    ) -> Result<Self, ErrorCode> {
        let ticket = HelperTicket {
            endpoint: ticket.endpoint.clone(),
            key: AttemptKey {
                consumer_id: ticket.consumer.clone(),
                consumer_generation: ticket.generation.clone(),
                attempt_id: ticket.attempt_id.clone(),
            },
            instance_id: ticket.instance_id.clone(),
        };
        let (command, report) = helper_command(helper, &ticket, &permit.0, program, args)
            .map_err(|error| ErrorCode::from(error.code))?;
        drop(permit);
        let args: Vec<OsString> = command.args().map(OsStr::to_owned).collect();
        let descriptor = |flag: &str| -> Option<RawFd> {
            let at = args.iter().position(|arg| arg == flag)?;
            args.get(at + 1)?.to_str()?.parse().ok()
        };
        // DevGuard states them as the helper's `--permit-fd` and `--report-fd`.
        let (Some(permit_fd), Some(report_fd)) =
            (descriptor("--permit-fd"), descriptor("--report-fd"))
        else {
            return Err(ErrorCode::InvalidRequest);
        };
        Ok(Self {
            helper: helper.to_owned(),
            args,
            descriptors: [permit_fd, report_fd],
            _command: command,
            report,
        })
    }

    /// The helper's absolute path.
    pub fn helper(&self) -> &Path {
        &self.helper
    }

    /// The helper's arguments, ending with `--`, the program and its arguments. None is secret.
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The two descriptors the helper must inherit, at these numbers, and the only ones besides
    /// its standard three: the permit carrier, then the transcript writer. Both are
    /// close-on-exec in this process.
    pub fn descriptors(&self) -> [RawFd; 2] {
        self.descriptors
    }

    /// Clear close-on-exec on both descriptors in this process, for a spawner that cannot clear
    /// it in its child (the pinned Codex PTY). They stay inheritable until
    /// [`Self::into_transcript`] closes them, so every other spawner in this process must keep
    /// unrelated descriptors out of its children meanwhile, as CodeSpace's do (#79).
    pub fn make_inheritable(&self) -> std::io::Result<()> {
        for fd in self.descriptors {
            // SAFETY: fcntl reads and changes only the flags of a descriptor this invocation
            // owns.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Close this process's copies of the permit carrier and the transcript writer, and keep
    /// the owner's end of the transcript. Call it as soon as the spawn has returned, whether or
    /// not it succeeded, so the transcript ends when the helper execs or exits.
    pub fn into_transcript(self) -> Transcript {
        Transcript(self.report)
    }
}

/// The owner's end of one helper's transcript.
pub struct Transcript(HelperReport);

/// Where a helper failed before presenting the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperStage {
    /// Leading its own process group.
    Scope,
    /// Re-executing under the utility QoS clamp.
    Clamp,
    /// Protecting the transcript from the executable.
    Report,
    /// Reading the permit.
    Permit,
    /// Reaching the authority.
    Connect,
    Other,
}

/// How a helper's transcript ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptEnd {
    /// READY, then the transcript closed: the helper exec'd the executable, which kept its PID.
    /// A helper killed between READY and its exec looks the same, so the exit status still
    /// decides what the executable did.
    Started,
    /// READY, then the exec failed with this errno; the helper exits 126, or 127 for `ENOENT`.
    ExecFailed { errno: i32 },
    /// The authority did not authorize the helper, or its answer was lost: the helper never
    /// attempted the executable. It may have claimed the grant first.
    Refused { code: ErrorCode },
    /// The helper could not present the grant, so it did not claim it, and never attempted the
    /// executable.
    Failed { stage: HelperStage },
    /// The transcript closed without READY or a report: the helper ended before READY, which it
    /// writes before its exec, so the executable never ran.
    EndedBeforeReady,
    /// READY without a final report, or a transcript that is not well formed: whether the
    /// executable started is not known.
    Uncertain,
}

impl Transcript {
    /// Read the transcript until the helper closes it: when its executable starts or when the
    /// helper exits. This blocks; the helper's own deadlines bound it up to READY.
    pub fn read(self) -> TranscriptEnd {
        match self.0.read_until_closed() {
            Ok((outcome, _)) => transcript_end(outcome),
            Err(_) => TranscriptEnd::Uncertain,
        }
    }
}

fn transcript_end(outcome: LaunchOutcome) -> TranscriptEnd {
    match outcome {
        LaunchOutcome::Started => TranscriptEnd::Started,
        LaunchOutcome::ExecFailed { errno } => TranscriptEnd::ExecFailed { errno },
        LaunchOutcome::NotStarted(HelperPhase::Refused { code, .. }) => {
            TranscriptEnd::Refused { code: code.into() }
        }
        LaunchOutcome::NotStarted(HelperPhase::Failed { stage, .. }) => TranscriptEnd::Failed {
            stage: match stage.as_str() {
                "scope" => HelperStage::Scope,
                "clamp" => HelperStage::Clamp,
                "report" => HelperStage::Report,
                "permit" => HelperStage::Permit,
                "connect" => HelperStage::Connect,
                _ => HelperStage::Other,
            },
        },
        LaunchOutcome::NotStarted(_) => TranscriptEnd::Uncertain,
        LaunchOutcome::Lost { ready: false } => TranscriptEnd::EndedBeforeReady,
        LaunchOutcome::Lost { ready: true } => TranscriptEnd::Uncertain,
    }
}

/// Why a launch helper cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperProblem {
    /// The path is not absolute, or names `.` or `..`.
    NotAbsolute,
    /// Nothing usable is there: missing, a link, or not a regular file.
    NotAFile,
    /// The file is not executable.
    NotExecutable,
    /// Another user than this one or root owns the file or a directory on its path, or others,
    /// or a group other than root's, can write it, so they could replace the helper this owner
    /// hands its permits to.
    NotPrivate,
}

impl HelperProblem {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotAbsolute => "the launch helper path is not absolute",
            Self::NotAFile => "the launch helper is not a regular file",
            Self::NotExecutable => "the launch helper is not executable",
            Self::NotPrivate => {
                "the launch helper, or a directory on its path, can be replaced by another user"
            }
        }
    }
}

/// Whether `path` can serve as the launch helper: an absolute path, through real directories
/// owned by this user or root, to a regular, executable file owned by this user or root, where
/// only their owner or root can write any of them (apart from a root-owned sticky `/tmp`): the
/// group may write only when it is root's group (gid 0, `wheel` on macOS). The owner hands this
/// file each permit, so no one else may swap it.
pub fn check_helper(path: &Path) -> Result<(), HelperProblem> {
    let (true, Some(parent)) = (path.is_absolute(), path.parent()) else {
        return Err(HelperProblem::NotAbsolute);
    };
    let uid = effective_uid();
    let owned = |meta: &std::fs::Metadata| meta.uid() == uid || meta.uid() == 0;
    // Others never write it; a group does only when it is root's.
    let shared = |meta: &std::fs::Metadata| {
        meta.mode() & 0o002 != 0 || (meta.mode() & 0o020 != 0 && meta.gid() != 0)
    };
    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir => current.push("/"),
            Component::Normal(name) => current.push(name),
            _ => return Err(HelperProblem::NotAbsolute),
        }
        let meta = std::fs::symlink_metadata(&current).map_err(|_| HelperProblem::NotPrivate)?;
        let shared_tmp = matches!(current.to_str(), Some("/tmp" | "/private/tmp"))
            && meta.uid() == 0
            && meta.mode() & 0o1000 != 0;
        if !meta.is_dir() || !owned(&meta) || (!shared_tmp && shared(&meta)) {
            return Err(HelperProblem::NotPrivate);
        }
    }
    if path
        .file_name()
        .is_none_or(|name| name == "." || name == "..")
    {
        return Err(HelperProblem::NotAbsolute);
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| HelperProblem::NotAFile)?;
    if !meta.is_file() {
        return Err(HelperProblem::NotAFile);
    }
    if !owned(&meta) || shared(&meta) {
        return Err(HelperProblem::NotPrivate);
    }
    if meta.mode() & 0o111 == 0 {
        return Err(HelperProblem::NotExecutable);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::admission::tests::admission;
    use crate::tests::{private_file, short_directory};
    use crate::{OwnerCredential, OwnerSettings};
    use std::os::unix::fs::PermissionsExt;

    /// DevGuard's launch helper built from the pinned source, named by
    /// `CODESPACE_DEVGUARD_LAUNCH_BIN`. Without it a test that needs it is skipped, unless
    /// `CODESPACE_REQUIRE_DEVGUARD_BINS` says the stage built it, which then fails the test.
    pub(crate) fn helper_bin() -> Option<PathBuf> {
        match std::env::var_os("CODESPACE_DEVGUARD_LAUNCH_BIN") {
            Some(path) => Some(PathBuf::from(path)),
            None if std::env::var_os("CODESPACE_REQUIRE_DEVGUARD_BINS").is_some() => {
                panic!("CODESPACE_DEVGUARD_LAUNCH_BIN is required by this stage")
            }
            None => {
                eprintln!("skipped: CODESPACE_DEVGUARD_LAUNCH_BIN is not set");
                None
            }
        }
    }

    #[test]
    fn a_permit_never_shows_itself() {
        let text = "ab".repeat(32);
        let permit = Permit::from_text(&text).unwrap();
        assert_eq!(format!("{permit:?}"), "Permit([REDACTED])");
        assert!(Permit::from_text("short").is_none());
        let answer = LaunchAnswer::Granted {
            attempt: crate::admission::Attempt {
                phase: Phase::LaunchCommitted,
                denial: None,
                plan: None,
                reservation: None,
                scope: None,
                applied: None,
                release: None,
                tracking_lost: false,
                known_not_started: false,
            },
            permit,
        };
        assert!(!format!("{answer:?}").contains(&text));
    }

    #[test]
    fn transcripts_keep_ready_apart_from_what_the_executable_did() {
        let refused = HelperPhase::Refused {
            code: contract::ErrorCode::InvalidTransition,
            message: "a message that is never kept".into(),
        };
        let failed = |stage: &str| HelperPhase::Failed {
            stage: stage.into(),
            message: "never kept".into(),
        };
        let cases = [
            (LaunchOutcome::Started, TranscriptEnd::Started),
            (
                LaunchOutcome::ExecFailed { errno: 8 },
                TranscriptEnd::ExecFailed { errno: 8 },
            ),
            (
                LaunchOutcome::NotStarted(refused),
                TranscriptEnd::Refused {
                    code: ErrorCode::InvalidTransition,
                },
            ),
            (
                LaunchOutcome::NotStarted(failed("permit")),
                TranscriptEnd::Failed {
                    stage: HelperStage::Permit,
                },
            ),
            (
                LaunchOutcome::NotStarted(failed("elsewhere")),
                TranscriptEnd::Failed {
                    stage: HelperStage::Other,
                },
            ),
            (
                LaunchOutcome::Lost { ready: false },
                TranscriptEnd::EndedBeforeReady,
            ),
            (
                LaunchOutcome::Lost { ready: true },
                TranscriptEnd::Uncertain,
            ),
        ];
        for (outcome, end) in cases {
            assert_eq!(transcript_end(outcome), end);
        }
    }

    fn owner(dir: &Path) -> Owner {
        let credential = private_file(dir, "owner.secret", "5ec2".repeat(16).as_bytes());
        Owner::new(
            OwnerSettings {
                socket: dir.join("authority.sock"),
                consumer: "codespace".into(),
                generation: "g1".into(),
            },
            OwnerCredential::File(credential),
        )
    }

    #[test]
    fn an_invocation_names_its_two_descriptors_and_carries_no_secret() {
        let dir = short_directory();
        let owner = owner(dir.path());
        let text = "c0".repeat(32);
        let invocation = owner
            .helper(
                Path::new("/usr/libexec/devguard-launch"),
                &admission("cs-attempt-h", &["/bin/echo"]),
                Permit::from_text(&text).unwrap(),
                Path::new("/bin/echo"),
                &["hi".into()],
            )
            .unwrap();
        let [permit_fd, report_fd] = invocation.descriptors();
        assert!(permit_fd > 2 && report_fd > 2 && permit_fd != report_fd);
        for fd in invocation.descriptors() {
            // SAFETY: reads the flags of a descriptor the invocation owns.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "{fd}");
        }
        let args: Vec<String> = invocation
            .args()
            .iter()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        let tail = ["--", "/bin/echo", "hi"].map(String::from);
        assert!(args.ends_with(&tail), "{args:?}");
        assert!(args.contains(&"cs-attempt-h".to_owned()));
        assert!(args.contains(&owner.instance_id().to_owned()));
        let shown = format!("{invocation:?} {args:?}");
        assert!(!shown.contains(&text), "{shown}");
        // Inheritable on request, and closed with the invocation.
        invocation.make_inheritable().unwrap();
        for fd in invocation.descriptors() {
            // SAFETY: as above.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
        let transcript = invocation.into_transcript();
        // The writer is closed, so the transcript ends at once without READY.
        assert_eq!(transcript.read(), TranscriptEnd::EndedBeforeReady);
        // A relative helper or program is refused before anything is created.
        for (helper, program) in [
            ("devguard-launch", "/bin/echo"),
            ("/x/devguard-launch", "echo"),
        ] {
            assert_eq!(
                owner
                    .helper(
                        Path::new(helper),
                        &admission("cs-attempt-h", &["/bin/echo"]),
                        Permit::from_text(&text).unwrap(),
                        Path::new(program),
                        &[],
                    )
                    .unwrap_err(),
                ErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn only_a_private_executable_file_serves_as_the_helper() {
        let dir = short_directory();
        let base = dir.path().join("bin");
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = |name: &str, mode: u32| {
            let path = base.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        assert_eq!(check_helper(&file("good", 0o700)), Ok(()));
        assert_eq!(check_helper(&file("ro", 0o500)), Ok(()));
        assert_eq!(
            check_helper(&file("plain", 0o600)),
            Err(HelperProblem::NotExecutable)
        );
        // A group other than root's may not write it. (A file created here takes the
        // directory's group, root's on macOS, so it is moved to this user's own group.)
        let group = file("group", 0o770);
        // SAFETY: getgid has no preconditions.
        let own_group = unsafe { libc::getgid() };
        std::os::unix::fs::chown(&group, None, Some(own_group)).unwrap();
        if own_group != 0 {
            assert_eq!(check_helper(&group), Err(HelperProblem::NotPrivate));
        }
        // Root's group writing it is root writing it.
        let wheel = file("wheel", 0o770);
        if std::fs::metadata(&wheel).unwrap().gid() == 0 {
            assert_eq!(check_helper(&wheel), Ok(()));
        }
        let link = base.join("link");
        std::os::unix::fs::symlink(base.join("good"), &link).unwrap();
        assert_eq!(check_helper(&link), Err(HelperProblem::NotAFile));
        assert_eq!(
            check_helper(&base.join("missing")),
            Err(HelperProblem::NotAFile)
        );
        assert_eq!(check_helper(&base), Err(HelperProblem::NotAFile));
        assert_eq!(
            check_helper(Path::new("bin/good")),
            Err(HelperProblem::NotAbsolute)
        );
        assert_eq!(
            check_helper(&base.join("../bin/good")),
            Err(HelperProblem::NotAbsolute)
        );
        // A directory others can write lets them replace the file.
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let exposed = open.join("helper");
        std::fs::write(&exposed, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exposed, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(check_helper(&exposed), Err(HelperProblem::NotPrivate));
    }

    // A scripted authority: answers a real one does not give at will.

    use crate::admission::tests::{scripted, scripted_owner, Reply};
    use crate::registration::RegistrationState;
    use devguard_client::protocol::{LaunchGrant, Request as Wire, Response, WireError};
    use devguard_contract::{AttemptRecord, InstanceIdentity};
    use std::sync::Arc;

    fn launched(owner: &Owner, admission: &Admission) -> LaunchAnswer {
        // DevGuard reads the credential within 250 ms (see `past_the_credential`).
        crate::tests::past_the_credential(
            |answer: &LaunchAnswer| {
                matches!(answer, LaunchAnswer::NotSent(registration)
                    if registration.state == RegistrationState::CredentialUnavailable)
            },
            || owner.begin_launch(admission),
        )
    }

    /// The committed record DevGuard keeps for `owner`'s attempt `key` with `fingerprint`.
    fn committed(key: &AttemptKey, fingerprint: &str, owner: &InstanceIdentity) -> AttemptRecord {
        AttemptRecord {
            key: key.clone(),
            request_fingerprint: fingerprint.to_owned(),
            owner: owner.clone(),
            policy_revision: "revision".into(),
            phase: contract::AttemptPhase::LaunchCommitted,
            reservation: None,
            plan: None,
            scope: None,
            applied: None,
            denial: None,
            tracking_lost: false,
            release_reason: None,
        }
    }

    #[test]
    fn only_a_permit_with_this_attempts_committed_record_is_a_grant() {
        let dir = short_directory();
        let text = "9a".repeat(32);
        let probe = scripted_owner(dir.path(), &dir.path().join("unused.sock"));
        let wire = probe.wire(&admission("cs-l1", &["/bin/echo"])).unwrap();
        let fingerprint = wire.fingerprint().unwrap();
        // Session by session: a grant; a grant without its permit, as for a replay; a grant
        // for another attempt; one for another meaning; an uncommitted record with a permit.
        let permit = text.clone();
        let socket = dir.path().join("grants.sock");
        let _authority = scripted(
            &socket,
            Arc::new(move |session, request, owner| {
                let Wire::BeginLaunch { key } = request else {
                    return Reply::Close;
                };
                let mut record = committed(key, &fingerprint, owner);
                let mut permit = Secret::new(permit.clone()).ok();
                match session {
                    0 => {}
                    1 => permit = None,
                    2 => record.key.attempt_id = "cs-other".into(),
                    3 => record.request_fingerprint = "0".repeat(64),
                    _ => record.phase = contract::AttemptPhase::Prepared,
                }
                Reply::With(Box::new(Response::LaunchGranted(LaunchGrant {
                    attempt: record,
                    permit,
                })))
            }),
        );
        let owner = scripted_owner(dir.path(), &socket);
        let admission = admission("cs-l1", &["/bin/echo"]);
        let LaunchAnswer::Granted { attempt, permit } = launched(&owner, &admission) else {
            panic!("not granted");
        };
        assert_eq!(attempt.phase, Phase::LaunchCommitted);
        assert_eq!(permit.0.expose(), text);
        for _ in 1..5 {
            assert!(matches!(
                launched(&owner, &admission),
                LaunchAnswer::Unknown
            ));
        }
    }

    #[test]
    fn a_refusal_commits_nothing_and_a_lost_answer_is_unknown() {
        let dir = short_directory();
        for code in crate::tests::CODES {
            let socket = dir.path().join(format!("{code:?}.sock"));
            let _authority = scripted(
                &socket,
                Arc::new(move |_, _, _| {
                    Reply::With(Box::new(Response::Error(WireError {
                        code,
                        message: "a message that is never kept".into(),
                    })))
                }),
            );
            let answer = launched(
                &scripted_owner(dir.path(), &socket),
                &admission("cs-l2", &["/bin/echo"]),
            );
            match code {
                // DevGuard's client's own code for a transport failure.
                contract::ErrorCode::ResourceControlUnavailable => {
                    assert!(matches!(answer, LaunchAnswer::Unknown), "{answer:?}")
                }
                code => assert!(
                    matches!(answer, LaunchAnswer::Refused(refused) if refused == code.into()),
                    "{answer:?}"
                ),
            }
            assert!(!format!("{answer:?}").contains("never kept"));
        }
        for (name, silent) in [("silent", true), ("lost", false)] {
            let socket = dir.path().join(format!("{name}.sock"));
            let authority = scripted(
                &socket,
                Arc::new(move |_, _, _| if silent { Reply::Silent } else { Reply::Close }),
            );
            let answer = launched(
                &scripted_owner(dir.path(), &socket),
                &admission("cs-l3", &["/bin/echo"]),
            );
            assert!(
                matches!(answer, LaunchAnswer::Unknown),
                "{name}: {answer:?}"
            );
            // The request reached the authority: it may have committed.
            assert_eq!(
                authority.requests.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
        }
        // Without a usable credential no session opens, so nothing was asked.
        let unusable = Owner::new(
            OwnerSettings {
                socket: dir.path().join("absent.sock"),
                consumer: "codespace".into(),
                generation: "g1".into(),
            },
            OwnerCredential::Unavailable,
        );
        assert!(matches!(
            unusable.begin_launch(&admission("cs-l4", &["/bin/echo"])),
            LaunchAnswer::NotSent(registration)
                if registration.state == RegistrationState::CredentialUnavailable
        ));
    }

    // DevGuard's fixture authority with native host evidence and its launch helper, as on macOS.

    #[cfg(target_os = "macos")]
    mod native {
        use super::*;
        use crate::admission::tests::request;
        use crate::admission::{Abandon, Application, Release};
        use crate::registration::tests::native::{provision, Provisioned};
        use devguard_contract::Budget;
        use std::os::unix::process::CommandExt;
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        fn admitting() -> Provisioned {
            let provisioned = provision(2);
            provisioned
                .authority
                .wait_until_admitting(Duration::from_secs(10))
                .unwrap();
            provisioned
        }

        fn admitted(owner: &Owner, admission: &Admission) {
            let answer = crate::tests::past_the_credential(
                |answer: &Answer| matches!(answer, Answer::NotSent(_)),
                || owner.admit(admission),
            );
            assert!(
                matches!(answer, Answer::Attempt(attempt) if attempt.phase == Phase::Prepared),
                "{answer:?}"
            );
        }

        fn granted(owner: &Owner, admission: &Admission) -> Permit {
            match launched(owner, admission) {
                LaunchAnswer::Granted { attempt, permit } => {
                    assert_eq!(attempt.phase, Phase::LaunchCommitted);
                    assert_eq!(attempt.scope, None);
                    permit
                }
                other => panic!("not granted: {other:?}"),
            }
        }

        fn looked_up(owner: &Owner, admission: &Admission) -> crate::admission::Attempt {
            match crate::tests::past_the_credential(
                |answer: &Answer| matches!(answer, Answer::NotSent(_)),
                || owner.lookup(admission),
            ) {
                Answer::Attempt(attempt) => attempt,
                other => panic!("not found: {other:?}"),
            }
        }

        fn abandoned(
            owner: &Owner,
            admission: &Admission,
            reason: Abandon,
        ) -> crate::admission::Attempt {
            match crate::tests::past_the_credential(
                |answer: &Answer| matches!(answer, Answer::NotSent(_)),
                || owner.abandon_launch(admission, reason),
            ) {
                Answer::Attempt(attempt) => attempt,
                other => panic!("no record: {other:?}"),
            }
        }

        /// Start the helper as a direct child of this process, passing exactly its two
        /// descriptors, as CodeSpace's own spawners do.
        fn spawn(invocation: &HelperInvocation, stdout: Stdio) -> Child {
            let mut command = Command::new(invocation.helper());
            command
                .args(invocation.args())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(Stdio::null());
            let keep = invocation.descriptors();
            // SAFETY: the step runs in the child and only clears close-on-exec with fcntl.
            unsafe {
                command.pre_exec(move || {
                    for fd in keep {
                        let flags = libc::fcntl(fd, libc::F_GETFD);
                        if flags < 0
                            || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            command.spawn().unwrap()
        }

        /// The attempt once DevGuard has released it, which its reconciler does within about a
        /// second of observing the scope's end.
        fn released(owner: &Owner, admission: &Admission) -> crate::admission::Attempt {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let attempt = looked_up(owner, admission);
                if attempt.phase == Phase::Released || Instant::now() > deadline {
                    return attempt;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        fn charged(provisioned: &Provisioned) -> Budget {
            provisioned.authority.committed().unwrap()
        }

        #[test]
        fn a_granted_launch_runs_the_executable_once_in_its_bound_scope() {
            let Some(helper) = helper_bin() else { return };
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-launch-1", &["/bin/echo", "hi"]);
            admitted(&owner, &admission);
            let permit = granted(&owner, &admission);
            // DevGuard's grant is one-time: another BeginLaunch answers without a permit, which
            // is never taken for a grant. CodeSpace's own launch never sends one.
            assert!(matches!(
                launched(&owner, &admission),
                LaunchAnswer::Unknown
            ));
            let invocation = owner
                .helper(
                    &helper,
                    &admission,
                    permit,
                    Path::new("/bin/echo"),
                    &["hi".into()],
                )
                .unwrap();
            let child = spawn(&invocation, Stdio::piped());
            let root = child.id();
            assert_eq!(invocation.into_transcript().read(), TranscriptEnd::Started);
            // The helper's process group is the scope, rooted at the helper's PID, which the
            // executable kept, and every resource was applied before READY.
            let running = looked_up(&owner, &admission);
            assert!(
                matches!(running.phase, Phase::RunAuthorized | Phase::Released),
                "{running:?}"
            );
            assert_eq!(running.scope.map(|scope| scope.root_pid), Some(root));
            let applied = running.applied.unwrap();
            assert_eq!(
                (applied.cpu, applied.memory, applied.pids),
                (
                    Application::Applied,
                    Application::Applied,
                    Application::Applied
                )
            );
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"hi\n");
            // Once the scope ended, DevGuard released it through its scope, not as unstarted.
            let ended = released(&owner, &admission);
            assert_eq!(
                (ended.phase, ended.release, ended.known_not_started),
                (Phase::Released, Some(Release::ScopeTerminated), false)
            );
            assert_eq!(charged(&provisioned), Budget::ZERO);
        }

        #[test]
        fn a_lost_grant_is_found_committed_and_released_as_never_received() {
            let provisioned = admitting();
            let owner = provisioned.owner();
            let admission = admission("cs-launch-2", &["/bin/echo"]);
            admitted(&owner, &admission);
            // Once the session is registered the authority's lock is held past the client's
            // deadline, so BeginLaunch waits behind it and the client gives up.
            let holder = std::sync::Mutex::new(None);
            let answer = crate::tests::past_the_credential(
                |answer: &LaunchAnswer| matches!(answer, LaunchAnswer::NotSent(_)),
                || {
                    owner.begin_launch_after(
                        &admission,
                        effective_uid(),
                        std::process::id(),
                        &|| {
                            let held = provisioned
                                .authority
                                .hold_authority(Duration::from_millis(900));
                            *holder.lock().unwrap() = Some(held);
                        },
                    )
                },
            );
            if let Some(held) = holder.lock().unwrap().take() {
                held.join().unwrap();
            }
            assert!(matches!(answer, LaunchAnswer::Unknown), "{answer:?}");
            // DevGuard committed it after the client gave up: committed, and no helper claimed it.
            let deadline = Instant::now() + Duration::from_secs(2);
            let found = loop {
                let attempt = looked_up(&owner, &admission);
                if attempt.phase != Phase::Prepared || Instant::now() > deadline {
                    break attempt;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            assert_eq!((found.phase, found.scope), (Phase::LaunchCommitted, None));
            // The permit never arrived, so this owner holds no helper and starts none.
            let settled = abandoned(&owner, &admission, Abandon::GrantNotReceived);
            assert_eq!(
                (settled.phase, settled.release, settled.known_not_started),
                (Phase::Released, Some(Release::NoHelperCreated), true)
            );
            assert_eq!(charged(&provisioned), Budget::ZERO);
        }

        #[test]
        fn a_helper_that_cannot_present_the_grant_never_runs_the_executable() {
            let Some(helper) = helper_bin() else { return };
            let provisioned = admitting();
            let owner = provisioned.owner();
            let marker = provisioned_marker(&provisioned);
            let argv = ["/usr/bin/touch", marker.to_str().unwrap()];
            let admission = admission("cs-launch-3", &argv);
            admitted(&owner, &admission);
            let permit = granted(&owner, &admission);
            // A ticket that names an endpoint no authority serves: the helper fails at
            // `connect`, before it could claim the grant.
            let ticket = LaunchTicket {
                endpoint: provisioned.settings.socket.with_file_name("absent.sock"),
                consumer: owner.consumer().into(),
                generation: owner.generation().into(),
                attempt_id: admission.attempt_id.clone(),
                instance_id: owner.instance_id().into(),
            };
            let invocation = HelperInvocation::new(
                &helper,
                &ticket,
                permit,
                Path::new(argv[0]),
                &[argv[1].into()],
            )
            .unwrap();
            let mut child = spawn(&invocation, Stdio::null());
            assert_eq!(
                invocation.into_transcript().read(),
                TranscriptEnd::Failed {
                    stage: HelperStage::Connect
                }
            );
            let status = child.wait().unwrap();
            assert_eq!(
                status.code(),
                Some(devguard_client::launch::NOT_AUTHORIZED_STATUS)
            );
            assert!(!marker.exists(), "the executable ran");
            // The helper was reaped before READY and claimed nothing: the owner's report
            // releases the grant as never started.
            let settled = abandoned(&owner, &admission, Abandon::HelperExited);
            assert_eq!(
                (settled.phase, settled.release, settled.known_not_started),
                (Phase::Released, Some(Release::NoHelperCreated), true)
            );
            assert_eq!(charged(&provisioned), Budget::ZERO);
        }

        #[test]
        fn an_exec_failure_after_ready_is_settled_by_its_scope() {
            let Some(helper) = helper_bin() else { return };
            let provisioned = admitting();
            let owner = provisioned.owner();
            // A program that exists but cannot be executed: the exec after READY fails.
            let program = provisioned_marker(&provisioned).with_file_name("not-executable");
            std::fs::write(&program, b"#!/bin/sh\n").unwrap();
            let admission = admission("cs-launch-4", &[program.to_str().unwrap()]);
            admitted(&owner, &admission);
            let permit = granted(&owner, &admission);
            let invocation = owner
                .helper(&helper, &admission, permit, &program, &[])
                .unwrap();
            let mut child = spawn(&invocation, Stdio::null());
            assert_eq!(
                invocation.into_transcript().read(),
                TranscriptEnd::ExecFailed {
                    errno: libc::EACCES
                }
            );
            assert_eq!(
                child.wait().unwrap().code(),
                Some(devguard_client::launch::EXEC_FAILED_STATUS)
            );
            // The helper claimed the grant before READY, so an owner report cannot release it;
            // only the observed end of its scope does.
            let reported = abandoned(&owner, &admission, Abandon::HelperExited);
            assert_ne!(reported.release, Some(Release::NoHelperCreated));
            let ended = released(&owner, &admission);
            assert_eq!(
                (ended.phase, ended.release, ended.known_not_started),
                (Phase::Released, Some(Release::ScopeTerminated), false)
            );
            assert_eq!(charged(&provisioned), Budget::ZERO);
        }

        #[test]
        fn the_executable_keeps_none_of_the_helpers_descriptors() {
            let Some(helper) = helper_bin() else { return };
            let provisioned = admitting();
            let owner = provisioned.owner();
            // The helper's descriptor numbers are known only once its invocation exists, after
            // the command was admitted, so the probe reports every open number up to a bound
            // above them.
            let script = "n=3; while [ $n -lt 1024 ]; do \
                          if [ -e /dev/fd/$n ]; then echo $n; fi; n=$((n + 1)); done; echo end";
            let argv = ["/bin/sh", "-c", script];
            let admission = admission("cs-launch-5", &argv);
            admitted(&owner, &admission);
            let permit = granted(&owner, &admission);
            let invocation = owner
                .helper(
                    &helper,
                    &admission,
                    permit,
                    Path::new(argv[0]),
                    &[argv[1].into(), argv[2].into()],
                )
                .unwrap();
            assert!(invocation.descriptors().iter().all(|fd| *fd < 1024));
            let child = spawn(&invocation, Stdio::piped());
            assert_eq!(invocation.into_transcript().read(), TranscriptEnd::Started);
            let output = child.wait_with_output().unwrap();
            // Neither the permit carrier, nor the transcript writer, nor the helper's session
            // socket reaches the executable.
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "end\n");
            assert_eq!(
                released(&owner, &admission).release,
                Some(Release::ScopeTerminated)
            );
        }

        /// A path in the provisioned authority's private directory.
        fn provisioned_marker(provisioned: &Provisioned) -> PathBuf {
            let base = provisioned.settings.socket.parent().unwrap().to_path_buf();
            base.join(format!("started-{}", std::process::id()))
        }

        #[test]
        fn the_default_request_fits_the_fixture() {
            // The launch tests ask what the admission tests ask.
            assert_eq!(request().requested.cpu_milli, 100);
        }
    }
}
