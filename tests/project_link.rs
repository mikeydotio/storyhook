//! The optional git layer: `story project link|unlink origin|checkout`
//! (SH-117, C5 of the server-owned epic SH-112).
//!
//! # Two associations, and every assertion here is about the difference
//!
//! An **origin** is the only thing project selection ever consults, and it
//! belongs to at most one project — so a collision is a loud refusal naming the
//! holder, and the freed identity becomes claimable the moment it is unlinked.
//!
//! A **checkout** is never *consulted* for resolution at all — `checkout_path`
//! is not, and has never been, a resolver input; `tests/checkout_path_readers.rs`
//! is what enforces that. It answers where this project's repo-side work
//! runs, there is at most one, and two projects may name the same directory:
//! that is a monorepo, not an ambiguity.
//!
//! What `link checkout` now *also* does (SH-167) is write the one artifact
//! that **is** a resolver input: the directory's `.storyhook.toml` pointer
//! file, when the directory has none of its own. A checkout link is still
//! never a claim on identity — a directory that already carries a *different*
//! project's pointer keeps resolving to it, untouched, which is what
//! `linking_a_checkout_that_names_another_project_leaves_its_pointer_alone`
//! pins.
//!
//! # The omitted URL is where SH-151 started
//!
//! `git config --get remote.origin.url` **walks up the directory tree**, so a
//! subdirectory of a repository reports the *enclosing* repository's origin.
//! Registration cannot use that answer: an origin belongs to at most one
//! project, so claiming the parent's identity from a child directory is
//! permanent and locks every sibling out. `link origin` with no URL therefore
//! requires the working directory to **own** the origin it reports, and
//! `an_omitted_url_refuses_inside_an_enclosing_repository` is the test that
//! would go green if somebody ever "simplified" that check away.
//!
//! SH-151 finished the job the other side of this file could not see: the
//! *explicit*-URL form is guarded too, `story project new` is guarded, and
//! ownership now means "the main working tree", not merely "a top level".
//! `tests/origin_ownership.rs` is where the whole rule and its layout table
//! live; what stays here is this verb's own surface.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir_named, slug_at};

/// The URL a fixture registers. Never fetched — every assertion here is about
/// what the store records, and a reachable remote would make these tests
/// depend on the network.
const URL: &str = "git@github.com:acme/widgets.git";

/// The same identity, spelled the other way. `link` and `unlink` normalize, so
/// one form must find a row the other wrote.
const URL_HTTPS: &str = "https://github.com/acme/widgets";

/// A project with no git repository behind it, and its slug.
///
/// Deliberately not a git fixture: `link origin <url>` names the URL outright
/// and must not need a repository to be standing in, which is what makes it
/// usable for a project whose checkout is on another machine.
fn project(env: &TestEnv) -> (tempfile::TempDir, String) {
    let dir = scratch_dir_named("link-");
    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    let slug = slug_at(env, dir.path());
    (dir, slug)
}

/// `story --project <slug> project <args…>`, run from `cwd`.
fn scoped(env: &TestEnv, cwd: &Path, slug: &str, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["--project", slug, "project"]);
    cmd.args(args);
    cmd.output().expect("running the command under test")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A git repository with `origin` set to `url`, at its own top level.
fn repo_with_origin(url: &str) -> tempfile::TempDir {
    let dir = scratch_dir_named("repo-");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["remote", "add", "origin", url]);
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("running git");
    assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
}

// ---------------------------------------------------------------------------
// link origin
// ---------------------------------------------------------------------------

#[test]
fn linking_an_origin_makes_a_checkout_of_it_resolve_without_a_flag() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);

    let out = scoped(&env, dir.path(), &slug, &["link", "origin", URL]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains(URL), "{}", stdout(&out));

    // The point of the whole verb: a *different* directory, which is a clone of
    // that origin and has never been initialized, now answers for the project.
    let clone = repo_with_origin(URL_HTTPS);
    let listed = env
        .story(clone.path())
        .args(["summary"])
        .output()
        .expect("running `story summary`");
    assert!(
        listed.status.success(),
        "a checkout of a registered origin must resolve: {}",
        stderr(&listed)
    );
}

#[test]
fn every_spelling_of_one_origin_is_the_same_registration() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    scoped(&env, dir.path(), &slug, &["link", "origin", URL]);

    // Re-registering the same identity in another spelling is a no-op success,
    // not a conflict with itself. That is what makes `link origin` safe in a
    // provisioning script that runs twice.
    let again = scoped(&env, dir.path(), &slug, &["link", "origin", URL_HTTPS]);
    assert!(again.status.success(), "{}", stderr(&again));
}

#[test]
fn an_origin_another_project_holds_is_refused_loudly_and_writes_nothing() {
    let env = TestEnv::isolated();
    let (holder_dir, holder) = project(&env);
    let (other_dir, other) = project(&env);
    scoped(&env, holder_dir.path(), &holder, &["link", "origin", URL]);

    let out = scoped(&env, other_dir.path(), &other, &["link", "origin", URL]);
    assert!(!out.status.success(), "the collision must refuse");
    assert_eq!(out.status.code(), Some(2), "a conflict is not corruption");
    let message = stderr(&out);
    assert!(
        message.contains(&holder),
        "the refusal must name the holding project: {message}"
    );
    assert!(
        message.contains("story project list"),
        "the refusal must name the way to find out who holds what: {message}"
    );

    // And the holder still holds it — a refused registration writes nothing.
    let clone = repo_with_origin(URL);
    let resolved = env
        .story(clone.path())
        .args(["project", "settings", "list"])
        .output()
        .expect("running `story project settings list`");
    assert!(
        resolved.status.success(),
        "the original registration must survive the refusal: {}",
        stderr(&resolved)
    );
}

#[test]
fn a_url_that_is_not_a_remote_is_refused_naming_it() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let out = scoped(&env, dir.path(), &slug, &["link", "origin", "https://"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("https://"), "{}", stderr(&out));
}

// ---------------------------------------------------------------------------
// The omitted URL, and the SH-151 guard on it
// ---------------------------------------------------------------------------

#[test]
fn an_omitted_url_takes_this_repositorys_own_origin() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    env.story(repo.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    let slug = slug_at(&env, repo.path());

    // `init` registers the origin itself now, so unlink first: this test is
    // about the omitted-URL form, not about arriving already registered.
    scoped(&env, repo.path(), &slug, &["unlink", "origin"]);

    let out = scoped(&env, repo.path(), &slug, &["link", "origin"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("acme/widgets"), "{}", stdout(&out));
}

/// **The SH-151 guard.** `git config --get remote.origin.url` walks up, so from
/// a subdirectory the omitted form would otherwise register the *enclosing*
/// repository's identity against this project — permanently, since an origin
/// belongs to at most one project, locking out every sibling project in that
/// repository.
///
/// The refusal used to say "not the top level of its repository". It says who
/// *owns* the origin now, because being a top level stopped being the whole
/// test when SH-151 made a linked worktree — which is its own top level, and
/// shares its main checkout's config — a non-owner too.
#[test]
fn an_omitted_url_refuses_inside_an_enclosing_repository() {
    let env = TestEnv::isolated();
    let repo = repo_with_origin(URL);
    let inner = repo.path().join("service-b");
    std::fs::create_dir_all(&inner).expect("creating the subdirectory");
    env.story(&inner)
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    let slug = slug_at(&env, &inner);

    let out = scoped(&env, &inner, &slug, &["link", "origin"]);
    assert!(
        !out.status.success(),
        "taking the enclosing repository's origin must refuse"
    );
    let message = stderr(&out);
    assert!(
        message.contains("does not own its repository's origin"),
        "the refusal must say why: {message}"
    );
    assert!(
        message.contains(&*repo.path().canonicalize().unwrap().to_string_lossy()),
        "and must name the directory that does own it: {message}"
    );
    assert!(
        message.contains("link origin <url>"),
        "the refusal must name the way out: {message}"
    );
}

#[test]
fn an_omitted_url_outside_a_repository_refuses_saying_so() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let out = scoped(&env, dir.path(), &slug, &["link", "origin"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("not inside a git repository"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_omitted_url_in_a_repository_with_no_origin_lists_what_there_is() {
    let env = TestEnv::isolated();
    let repo = scratch_dir_named("repo-");
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(
        repo.path(),
        &["remote", "add", "upstream", "https://example.test/o/r"],
    );
    env.story(repo.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    let slug = slug_at(&env, repo.path());

    let out = scoped(&env, repo.path(), &slug, &["link", "origin"]);
    assert!(!out.status.success());
    let message = stderr(&out);
    assert!(
        message.contains("upstream"),
        "a repository with remotes but no `origin` must show what it does have: {message}"
    );
}

// ---------------------------------------------------------------------------
// unlink origin
// ---------------------------------------------------------------------------

#[test]
fn unlinking_an_origin_frees_it_for_another_project() {
    let env = TestEnv::isolated();
    let (first_dir, first) = project(&env);
    let (second_dir, second) = project(&env);
    scoped(&env, first_dir.path(), &first, &["link", "origin", URL]);

    // Unlinking by an equivalent spelling must find the row the other wrote.
    let out = scoped(
        &env,
        first_dir.path(),
        &first,
        &["unlink", "origin", URL_HTTPS],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let claimed = scoped(&env, second_dir.path(), &second, &["link", "origin", URL]);
    assert!(
        claimed.status.success(),
        "the freed identity must be claimable: {}",
        stderr(&claimed)
    );
}

#[test]
fn unlinking_an_origin_this_project_does_not_hold_refuses_naming_who_does() {
    let env = TestEnv::isolated();
    let (holder_dir, holder) = project(&env);
    let (other_dir, other) = project(&env);
    scoped(&env, holder_dir.path(), &holder, &["link", "origin", URL]);

    let out = scoped(&env, other_dir.path(), &other, &["unlink", "origin", URL]);
    assert!(!out.status.success());
    assert_eq!(
        out.status.code(),
        Some(3),
        "a URL this project does not hold is not-found, not a usage error"
    );
    assert!(stderr(&out).contains(&holder), "{}", stderr(&out));
}

// ---------------------------------------------------------------------------
// link / unlink checkout
// ---------------------------------------------------------------------------

#[test]
fn linking_a_checkout_writes_the_pointer_that_makes_it_resolve() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let elsewhere = scratch_dir_named("checkout-");

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &elsewhere.path().display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // The checkout_path link genuinely landed — asserted first, because
    // without it the invariant below would hold just as well against a verb
    // that does nothing.
    let listing = stdout(
        &env.story(dir.path())
            .args(["project", "list"])
            .output()
            .expect("running `story project list`"),
    );
    let canonical = elsewhere.path().canonicalize().unwrap();
    assert!(
        listing.contains(&format!("checkout  {}", canonical.display())),
        "`project list` must report the linked checkout:\n{listing}"
    );

    // **The invariant this test exists to pin (SH-167).** `link checkout`
    // writes the pointer that lets a bare story id resolve at the directory
    // it names, when that directory had none of its own.
    assert!(
        stdout(&out).contains("this directory now resolves"),
        "the report must say the directory now resolves: {}",
        stdout(&out)
    );
    assert!(
        elsewhere.path().join(".storyhook.toml").is_file(),
        "linking an unclaimed directory must write its pointer file"
    );
    let resolved = env
        .story(elsewhere.path())
        .args(["summary"])
        .output()
        .expect("running `story summary`");
    assert!(
        resolved.status.success(),
        "a linked checkout with no pointer of its own must now resolve: {}",
        stderr(&resolved)
    );
}

/// Re-running `link checkout` against an already-correct pointer must not
/// rewrite it — a `story` command that dirties a tracked file on every run is
/// its own defect class, distinct from whether the link itself is idempotent.
#[test]
fn linking_a_checkout_twice_writes_the_pointer_only_once() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let elsewhere = scratch_dir_named("checkout-");
    let pointer_path = elsewhere.path().join(".storyhook.toml");
    let target = elsewhere.path().display().to_string();

    let first = scoped(&env, dir.path(), &slug, &["link", "checkout", &target]);
    assert!(first.status.success(), "{}", stderr(&first));
    let bytes_after_first =
        std::fs::read(&pointer_path).expect("the pointer must exist after the first link");

    let second = scoped(&env, dir.path(), &slug, &["link", "checkout", &target]);
    assert!(second.status.success(), "{}", stderr(&second));
    let bytes_after_second =
        std::fs::read(&pointer_path).expect("the pointer must still exist after the second link");

    assert_eq!(
        bytes_after_first, bytes_after_second,
        "re-linking an already-correct checkout must not rewrite the pointer"
    );
    assert!(
        stdout(&second).contains("already named this project"),
        "the report should say nothing needed to change: {}",
        stdout(&second)
    );
}

/// `ProjectPointer`'s `[plugin]`/`[hooks]` tables are user-authored, and
/// storyhook has promised never to write them (`ProjectPointer`'s own doc
/// comment). Repairing a stale `prefix` must not disturb them.
#[test]
fn linking_a_checkout_preserves_user_authored_plugin_and_hooks_tables() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let uuid = pointer_uuid(&dir.path().join(".storyhook.toml"));

    let elsewhere = scratch_dir_named("checkout-");
    let pointer_path = elsewhere.path().join(".storyhook.toml");
    std::fs::write(
        &pointer_path,
        format!(
            "schema = 1\nuuid = \"{uuid}\"\nprefix = \"OLD\"\n\n[plugin]\nenabled = true\n\n\
             [hooks]\ntimeout_ms = 5000\n"
        ),
    )
    .expect("writing the hand-authored pointer");

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &elsewhere.path().display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("repaired")
            && stdout(&out).contains("`OLD`")
            && stdout(&out).contains("`SH`"),
        "the report must name the repair: {}",
        stdout(&out)
    );

    let after: toml::Value = toml::from_str(
        &std::fs::read_to_string(&pointer_path).expect("reading the repaired pointer"),
    )
    .expect("the repaired pointer must still be valid TOML");
    assert_eq!(
        after["prefix"].as_str(),
        Some("SH"),
        "the stale prefix must be repaired: {after:?}"
    );
    assert_eq!(after["uuid"].as_str(), Some(uuid.as_str()));
    assert_eq!(
        after["plugin"]["enabled"].as_bool(),
        Some(true),
        "the [plugin] table must survive a prefix repair: {after:?}"
    );
    assert_eq!(
        after["hooks"]["timeout_ms"].as_integer(),
        Some(5000),
        "the [hooks] table must survive a prefix repair: {after:?}"
    );
}

/// The uuid a project's own pointer file carries.
fn pointer_uuid(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("reading the pointer file");
    let value: toml::Value = toml::from_str(&text).expect("parsing the pointer file");
    value["uuid"]
        .as_str()
        .expect("a pointer file must carry a uuid")
        .to_string()
}

/// **The monorepo case (SH-151's shape, SH-167's constraint).** A directory
/// that already carries a *different* project's pointer keeps resolving to
/// it — a checkout link records where repo-side work runs, never a claim on
/// the directory's identity. Refusing here would also contradict
/// `tests/project_path_hygiene.rs`'s existing pin that this exact shape
/// succeeds at exit 0.
#[test]
fn linking_a_checkout_that_names_another_project_leaves_its_pointer_alone() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let (other_dir, _other_slug) = project(&env);
    env.story(other_dir.path())
        .args(["new", "a story only the other project has"])
        .assert()
        .success();
    let other_pointer = std::fs::read_to_string(other_dir.path().join(".storyhook.toml"))
        .expect("reading the other project's pointer");

    let elsewhere = scratch_dir_named("checkout-");
    std::fs::write(elsewhere.path().join(".storyhook.toml"), &other_pointer)
        .expect("planting the other project's pointer");

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &elsewhere.path().display().to_string()],
    );
    assert!(
        out.status.success(),
        "a checkout link is not a claim on identity: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("was left alone"),
        "the report must say the other project's pointer was left alone: {}",
        stdout(&out)
    );

    let after = std::fs::read_to_string(elsewhere.path().join(".storyhook.toml"))
        .expect("reading the pointer after linking");
    assert_eq!(
        after, other_pointer,
        "the other project's pointer must be byte-identical after a checkout link"
    );

    // The load-bearing half: the directory still resolves to the project its
    // own pointer names, not to the one that just linked it as a checkout.
    let listing = stdout(
        &env.story(elsewhere.path())
            .args(["list"])
            .output()
            .expect("running `story list`"),
    );
    assert!(
        listing.contains("a story only the other project has"),
        "linking must never switch which project a directory resolves to: {listing}"
    );
}

/// A pointer that will not even parse is refused before anything is written —
/// there is no way to tell "mine, just stale" from "someone else's" through a
/// syntax error, and guessing wrong in either direction is worse than asking.
#[test]
fn linking_a_checkout_with_an_unparseable_pointer_refuses_and_writes_nothing() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let elsewhere = scratch_dir_named("checkout-");
    std::fs::write(
        elsewhere.path().join(".storyhook.toml"),
        "this is not valid toml {{{",
    )
    .expect("planting a broken pointer");

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &elsewhere.path().display().to_string()],
    );
    assert!(!out.status.success(), "an unparseable pointer must refuse");
    assert_eq!(
        out.status.code(),
        Some(5),
        "a damaged file, not a usage mistake"
    );
    assert!(
        stderr(&out).contains(".storyhook.toml"),
        "the refusal must name the file: {}",
        stderr(&out)
    );

    // A refusal here must not have half-linked: the checkout_path write
    // happens *after* the pointer is read, so this project's checkout must
    // still be whatever it was before (its own directory, from `project
    // new`'s default attach) — never the broken-pointer directory.
    let listing = stdout(
        &env.story(dir.path())
            .args(["project", "list"])
            .output()
            .expect("running `story project list`"),
    );
    let broken = elsewhere.path().canonicalize().unwrap();
    assert!(
        !listing.contains(&broken.display().to_string()),
        "a refusal must not have recorded the broken-pointer directory as the checkout:\n{listing}"
    );
}

/// The pointer write is best-effort: the checkout link itself still succeeds,
/// and the failure is reported rather than silently swallowed or propagated.
/// The genuine escape hatch a `--no-pointer` flag would serve — a directory
/// storyhook cannot write into — is this degraded path, not a flag asking for
/// the unreachable state SH-119 already deleted a switch to prevent.
#[test]
#[cfg(unix)]
fn linking_a_checkout_reports_a_pointer_it_could_not_write() {
    use std::os::unix::fs::PermissionsExt;

    // Root ignores the permission bits this test relies on to make the
    // directory unwritable, which would turn the setup below into a no-op.
    if std::env::var_os("USER").as_deref() == Some(std::ffi::OsStr::new("root")) {
        eprintln!("skipping: running as root, which ignores directory write permissions");
        return;
    }

    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let elsewhere = scratch_dir_named("checkout-");
    std::fs::set_permissions(elsewhere.path(), std::fs::Permissions::from_mode(0o555))
        .expect("making the directory read-only");

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &elsewhere.path().display().to_string()],
    );

    // Restored before the TempDir's own cleanup runs, or that cleanup fails.
    std::fs::set_permissions(elsewhere.path(), std::fs::Permissions::from_mode(0o755))
        .expect("restoring permissions for cleanup");

    assert!(
        out.status.success(),
        "the checkout link itself must still succeed even though the pointer could not be \
         written: {}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("could not be written"),
        "the report must say the pointer could not be written: {}",
        stdout(&out)
    );

    let listing = stdout(
        &env.story(dir.path())
            .args(["project", "list"])
            .output()
            .expect("running `story project list`"),
    );
    let canonical = elsewhere.path().canonicalize().unwrap();
    assert!(
        listing.contains(&format!("checkout  {}", canonical.display())),
        "the checkout_path write does not depend on whether the pointer write succeeded:\n\
         {listing}"
    );
}

/// `project list` is where an origin registration becomes visible, and the
/// counterpart to the checkout assertion above.
#[test]
fn project_list_reports_the_origins_a_project_holds() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    scoped(&env, dir.path(), &slug, &["link", "origin", URL]);

    let listing = stdout(
        &env.story(dir.path())
            .args(["project", "list"])
            .output()
            .expect("running `story project list`"),
    );
    assert!(
        listing.contains(&format!("origin    {URL}")),
        "`project list` must report the registered origin:\n{listing}"
    );
}

#[test]
fn linking_a_second_checkout_reports_the_one_it_replaced() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let old = scratch_dir_named("old-");
    let new = scratch_dir_named("new-");
    scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &old.path().display().to_string()],
    );

    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &new.path().display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let message = stdout(&out);
    assert!(
        message.contains("replacing"),
        "a silent replacement is how somebody finds out weeks later: {message}"
    );
}

#[test]
fn linking_a_checkout_that_is_not_there_refuses_naming_the_path() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let out = scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", "/no/such/dir"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("/no/such/dir"), "{}", stderr(&out));
}

/// **The monorepo case, the other direction.** A subdirectory of a
/// repository whose root already carries a different project's pointer is
/// itself unclaimed. Linking a second project's checkout there writes a
/// pointer for the subdirectory alone — the ancestor walk stops at the
/// nearest pointer (`src/invoke.rs`'s `resolve_project`), so the
/// subdirectory now resolves to the project just linked there while the
/// repository root keeps resolving to the one it already named.
#[test]
fn linking_a_subdirectory_of_another_repository_makes_only_that_subdirectory_resolve() {
    let env = TestEnv::isolated();
    let (root_dir, _root_slug) = project(&env);
    env.story(root_dir.path())
        .args(["new", "a story at the repository root"])
        .assert()
        .success();

    let (sub_project_dir, sub_slug) = project(&env);
    let subdirectory = root_dir.path().join("service-b");
    std::fs::create_dir_all(&subdirectory).expect("creating the subdirectory");

    let out = scoped(
        &env,
        sub_project_dir.path(),
        &sub_slug,
        &["link", "checkout", &subdirectory.display().to_string()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        subdirectory.join(".storyhook.toml").is_file(),
        "the unclaimed subdirectory must get its own pointer"
    );

    // The subdirectory resolves to the project just linked there...
    let sub_listing = stdout(
        &env.story(&subdirectory)
            .args(["list"])
            .output()
            .expect("running `story list` in the subdirectory"),
    );
    assert!(
        !sub_listing.contains("a story at the repository root"),
        "the subdirectory must not resolve to the root project's stories: {sub_listing}"
    );

    // ...and the repository root is completely unaffected.
    let root_listing = stdout(
        &env.story(root_dir.path())
            .args(["list"])
            .output()
            .expect("running `story list` at the repository root"),
    );
    assert!(
        root_listing.contains("a story at the repository root"),
        "the repository root must keep resolving to its own project: {root_listing}"
    );
}

#[test]
fn unlinking_a_checkout_reports_what_went_and_is_safe_when_there_was_none() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);

    // Creating a project attached to a directory records that directory as the
    // checkout, so the empty slot has to be *made* rather than assumed. This
    // first unlink is the setup; the second one is the case under test.
    scoped(&env, dir.path(), &slug, &["unlink", "checkout"]);

    // Nothing linked: a success reporting so, unlike `unlink origin`. There is
    // exactly one checkout slot, so "there was nothing there" is unambiguous
    // and the caller cannot have removed the wrong one.
    let empty = scoped(&env, dir.path(), &slug, &["unlink", "checkout"]);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(
        stdout(&empty).contains("no linked checkout"),
        "{}",
        stdout(&empty)
    );

    let checkout = scratch_dir_named("checkout-");
    scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &checkout.path().display().to_string()],
    );
    let out = scoped(&env, dir.path(), &slug, &["unlink", "checkout"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&*checkout.path().to_string_lossy())
            || stdout(&out).contains("unlinked checkout"),
        "{}",
        stdout(&out)
    );
}

/// **Never deletes the pointer file (SH-167).** `.storyhook.toml` is
/// committed to the repository and may carry user-authored `[plugin]`/
/// `[hooks]` tables; deleting it on unlink would propagate to every other
/// clone on their next pull, and destroy configuration this service has
/// promised never to write, let alone remove. `unlink` answers "where does
/// *this machine* run repo-side work" — it is not a statement about the
/// repository's identity, so it must not touch the file that carries it.
#[test]
fn unlinking_a_checkout_leaves_the_pointer_file_in_place() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let checkout = scratch_dir_named("checkout-");
    let pointer_path = checkout.path().join(".storyhook.toml");

    scoped(
        &env,
        dir.path(),
        &slug,
        &["link", "checkout", &checkout.path().display().to_string()],
    );
    assert!(
        pointer_path.is_file(),
        "fixture: linking must have written the pointer"
    );
    let before = std::fs::read(&pointer_path).expect("reading the pointer before unlinking");

    let out = scoped(&env, dir.path(), &slug, &["unlink", "checkout"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("left in place"),
        "the report must say the pointer was left in place: {}",
        stdout(&out)
    );

    assert!(
        pointer_path.is_file(),
        "unlinking a checkout must not delete its pointer file"
    );
    let after = std::fs::read(&pointer_path).expect("reading the pointer after unlinking");
    assert_eq!(
        before, after,
        "unlinking must not even rewrite the pointer file"
    );

    // The directory still resolves — the pointer, not the checkout_path
    // column, is what resolution reads, and unlinking only cleared the latter.
    let resolved = env
        .story(checkout.path())
        .args(["summary"])
        .output()
        .expect("running `story summary`");
    assert!(
        resolved.status.success(),
        "the directory must still resolve after its checkout link (not its pointer) is \
         removed: {}",
        stderr(&resolved)
    );
}

/// `unlink checkout` takes no path, because a project has at most one. A caller
/// who types one believes otherwise and is about to be surprised by which one
/// went, so it is refused rather than ignored.
#[test]
fn unlink_checkout_refuses_a_path() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let out = scoped(&env, dir.path(), &slug, &["unlink", "checkout", "/tmp"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// Naming, scoping and the flag gate
// ---------------------------------------------------------------------------

/// These verbs name an existing project, so they obey SH-116's rules and no
/// fourth one. In a directory that resolves to nothing, the refusal is the same
/// single constructor every other scoped command uses.
#[test]
fn a_link_with_no_resolvable_project_refuses_the_way_everything_else_does() {
    let env = TestEnv::isolated();
    let nowhere = scratch_dir_named("nowhere-");
    let out = env
        .story(nowhere.path())
        .args(["project", "link", "origin", URL])
        .output()
        .expect("running the command under test");
    assert!(!out.status.success());
    let message = stderr(&out);
    assert!(
        message.contains("--project"),
        "the refusal must name both ways out: {message}"
    );
    assert!(
        !message.contains("unknown command"),
        "the verb must exist: {message}"
    );
}

/// SH-62's fail-closed gate covers a verb whose flag list is empty just as it
/// covers one with entries — and `project link` declares an empty list rather
/// than having no entry, so this is a declaration being enforced rather than a
/// hole being papered over.
#[test]
fn link_refuses_a_flag_shaped_token() {
    let env = TestEnv::isolated();
    let (dir, slug) = project(&env);
    let out = scoped(&env, dir.path(), &slug, &["link", "origin", "--typo"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--typo"), "{}", stderr(&out));
}

/// The naming hazard the story calls out: `story link` is an alias for `story
/// relate` and joins one *story* to another. `story project link` is a
/// different verb about git, and neither may shadow the other.
#[test]
fn story_link_still_relates_stories_and_is_not_project_link() {
    let env = TestEnv::isolated();
    let (dir, _slug) = project(&env);
    env.story(dir.path())
        .args(["new", "first"])
        .assert()
        .success();
    env.story(dir.path())
        .args(["new", "second"])
        .assert()
        .success();

    env.story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    let out = env
        .story(dir.path())
        .args(["show", "SH-2"])
        .output()
        .expect("running `story show`");
    assert!(
        stdout(&out).contains("blocked-by"),
        "`story link` must still relate stories: {}",
        stdout(&out)
    );
}
