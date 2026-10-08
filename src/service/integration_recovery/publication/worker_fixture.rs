//! Stateful remote responses for the actual native publication sequence.
//! No Store claims, real remote transport, or fabricated gate evidence live here.
use super::*;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Fault {
    #[default]
    None,
    PushReplyLost,
    CreateReplyLost,
    MovedAfterPush,
    MalformedManagedPr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub branch_head: Option<String>,
    pub managed_created: bool,
    pub push_calls: usize,
    pub create_calls: usize,
    pub requests: Vec<String>,
}

struct Binding {
    assembly: AssemblyEvidence,
    publication_epoch: u32,
}
struct State {
    binding: Option<Binding>,
    snapshot: Snapshot,
}

/// Test-only in-memory remote. The first real publication binds its actual
/// assembly once; subsequent calls cannot silently replace that owner or tree.
pub(crate) struct Remote {
    original: SubmissionObservation,
    managed_number: u64,
    fault: Fault,
    state: Mutex<State>,
}
impl Remote {
    pub(crate) fn new(
        original: SubmissionObservation,
        managed_number: u64,
        fault: Fault,
    ) -> Result<Self, AppError> {
        let source = crate::domain::pr_url::parse_pr_url(&original.pull_request)?;
        if managed_number == 0 || managed_number == source.number {
            return Err(refuse("fixture managed PR must be distinct"));
        }
        Ok(Self {
            original,
            managed_number,
            fault,
            state: Mutex::new(State {
                binding: None,
                snapshot: Snapshot {
                    branch_head: None,
                    managed_created: false,
                    push_calls: 0,
                    create_calls: 0,
                    requests: vec![],
                },
            }),
        })
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }

    /// This is the production publisher, including origin reads, private Git
    /// checks, effect CAS, raw reply parsing and native receipt construction.
    pub(crate) fn publish<S: Store>(
        &self,
        service: &IntegrationOwnerService<'_, S>,
        claim: &mut PublicationClaim,
        proof: &BoundIntegrationProposal,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<NativePublication, AppError> {
        if claim.submission() != &self.original {
            return Err(refuse("fixture original submission changed"));
        }
        {
            let mut state = self.state.lock().unwrap();
            match &state.binding {
                Some(binding)
                    if binding.assembly != *claim.assembly()
                        || binding.publication_epoch != claim.epoch() =>
                {
                    return Err(refuse("fixture publication owner changed"));
                }
                Some(_) => {}
                None => {
                    state.binding = Some(Binding {
                        assembly: claim.assembly().clone(),
                        publication_epoch: claim.epoch(),
                    });
                }
            }
        }
        publish_with_transport(service, claim, proof, deadline, cancellation, self)
    }

    fn live(
        &self,
        repository: &Repository,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), AppError> {
        if cancelled() || Instant::now() >= deadline {
            return Err(refuse("fixture transport owner expired or cancelled"));
        }
        if repository.qualified() != self.original.repository {
            return Err(refuse("fixture transport repository differs"));
        }
        Ok(())
    }

    fn pr(&self, assembly: Option<&Binding>) -> Result<Vec<u8>, AppError> {
        let source = crate::domain::pr_url::parse_pr_url(&self.original.pull_request)?;
        let repository = format!("{}/{}", source.owner, source.repo);
        let (number, url, head, branch, body) = if let Some(binding) = assembly {
            let a = &binding.assembly;
            let marker = format!(
                "<!-- storyhook-integration-owner:{}:{}:{} -->",
                a.owner, binding.publication_epoch, a.stamp_sha256
            );
            let (prefix, _) = self.original.pull_request.rsplit_once('/').unwrap();
            (
                self.managed_number,
                format!("{prefix}/{}", self.managed_number),
                a.commit.clone(),
                a.branch.clone(),
                Some(marker),
            )
        } else {
            (
                source.number,
                self.original.pull_request.clone(),
                self.original.head.clone(),
                "author".into(),
                None,
            )
        };
        serde_json::to_vec(&serde_json::json!({
            "number": number, "html_url": url, "state": "open", "merged": false,
            "body": body,
            "base": {"sha": self.original.base, "ref": self.original.base_branch, "repository": repository},
            "head": {"sha": head, "ref": branch, "repository": repository}
        }))
        .map_err(|e| refuse(&e.to_string()))
    }
}

impl PublicationTransport for Remote {
    fn git(
        &self,
        repository: &Repository,
        arguments: &[String],
        objects: Option<&Path>,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        self.live(repository, deadline, cancelled)?;
        let mut state = self.state.lock().unwrap();
        let binding = state
            .binding
            .as_ref()
            .ok_or_else(|| refuse("fixture unbound"))?;
        let assembly = &binding.assembly;
        let reference = format!("refs/heads/{}", assembly.branch);
        if arguments == strings(&["ls-remote", "--heads", "origin", &reference])
            && objects.is_none()
        {
            state.snapshot.requests.push("read-branch".into());
            return Ok(state
                .snapshot
                .branch_head
                .as_ref()
                .map_or_else(Vec::new, |head| {
                    format!("{head}\t{reference}\n").into_bytes()
                }));
        }
        if arguments
            != strings(&[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "origin",
                &format!("{}:{reference}", assembly.commit),
            ])
            || objects != Some(assembly.workspace.path.join("objects").as_path())
        {
            return Err(refuse("unexpected fixture Git operation"));
        }
        if state.snapshot.branch_head.is_some() || state.snapshot.push_calls != 0 {
            return Err(refuse("fixture branch push was replayed"));
        }
        let head = if self.fault == Fault::MovedAfterPush {
            let digit = if assembly.commit.bytes().all(|b| b == b'f') {
                'e'
            } else {
                'f'
            };
            digit.to_string().repeat(assembly.commit.len())
        } else {
            assembly.commit.clone()
        };
        state.snapshot.requests.push("push".into());
        state.snapshot.push_calls += 1;
        state.snapshot.branch_head = Some(head);
        if self.fault == Fault::PushReplyLost {
            return Err(refuse("fixture push took effect but reply was lost"));
        }
        Ok(Vec::new())
    }

    fn gh(
        &self,
        repository: &Repository,
        arguments: &[String],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        self.live(repository, deadline, cancelled)?;
        let source = crate::domain::pr_url::parse_pr_url(&self.original.pull_request)?;
        let mut state = self.state.lock().unwrap();
        let binding = state
            .binding
            .as_ref()
            .ok_or_else(|| refuse("fixture unbound"))?;
        if arguments
            == strings(&[
                "api",
                &endpoint(repository, &format!("pulls/{}", source.number)),
                "--jq",
                PR_FIELDS,
            ])
        {
            state.snapshot.requests.push("read-original-pr".into());
            return self.pr(None);
        }
        if arguments
            == strings(&[
                "api",
                &endpoint(repository, &format!("pulls/{}", self.managed_number)),
                "--jq",
                PR_FIELDS,
            ])
        {
            if !state.snapshot.managed_created {
                return Err(refuse("fixture managed PR has not been created"));
            }
            let answer = self.pr(Some(binding))?;
            state.snapshot.requests.push("read-managed-pr".into());
            return Ok(answer);
        }
        let a = &binding.assembly;
        let marker = format!(
            "<!-- storyhook-integration-owner:{}:{}:{} -->",
            a.owner, binding.publication_epoch, a.stamp_sha256
        );
        let expected = strings(&[
            "api",
            &endpoint(repository, "pulls"),
            "--method",
            "POST",
            "--raw-field",
            &format!("title=Managed integration {}", a.owner),
            "--raw-field",
            &format!("head={}", a.branch),
            "--raw-field",
            &format!("base={}", self.original.base_branch),
            "--raw-field",
            &format!(
                "body={marker}\n\nManaged integration of {} at {}. This PR requires independent certification and landing.",
                self.original.pull_request, self.original.head
            ),
            "--jq",
            PR_FIELDS,
        ]);
        if arguments != expected || state.snapshot.branch_head.as_ref() != Some(&a.commit) {
            return Err(refuse(
                "unexpected fixture PR operation or absent exact branch",
            ));
        }
        if state.snapshot.managed_created || state.snapshot.create_calls != 0 {
            return Err(refuse("fixture PR creation was replayed"));
        }
        let answer = self.pr(Some(binding))?;
        state.snapshot.requests.push("create-pr".into());
        state.snapshot.create_calls += 1;
        state.snapshot.managed_created = true;
        match self.fault {
            Fault::CreateReplyLost => Err(refuse("fixture PR created but reply was lost")),
            Fault::MalformedManagedPr => Ok(b"{}".to_vec()),
            _ => Ok(answer),
        }
    }
}
