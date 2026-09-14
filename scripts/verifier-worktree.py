#!/usr/bin/env python3
"""Recover one owned verifier checkout without discarding diagnostic state."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid

sys.dont_write_bytecode = True
from verifier_state import Refusal, atomic, held, paths, read, save, sync_dir


class Workspace:
    """Own reciprocal Git mappings and a restartable mutation journal."""

    def __init__(self, common, worktree, key):
        self.common, self.worktree = common, worktree
        self.archived = False
        self.path = Path(str(key) + ".state")
        self.canonical = common / "worktrees/verification-worktree"
        self.managed = worktree == common / "storyhook/verification-worktree"
        self.state = read(self.path) or {"version": 1, "common": str(common),
                                       "worktree": str(worktree)}
        if self.state.get("common") != str(common) or self.state.get("worktree") != str(worktree):
            raise Refusal(f"workspace mapping conflict in {self.path}")

    def persist(self):
        """Record intent before any multi-file transition."""
        save(self.path, self.state)

    def git(self, *args, private=None, check=True):
        """Use explicit administration and objects, never ambient Git overrides."""
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        for name in ("GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_SYSTEM"):
            if name in os.environ:
                env[name] = os.environ[name]
        if private:
            env["GIT_DIR"] = str(private / ".git")
            env["GIT_WORK_TREE"] = str(self.worktree)
            env["GIT_OBJECT_DIRECTORY"] = str(private / "objects")
            env["GIT_ALTERNATE_OBJECT_DIRECTORIES"] = str(self.common / "objects")
        result = subprocess.run(["git", "-C", str(self.worktree if self.worktree.exists() else self.common), *args],
                                env=env, text=True, capture_output=True)
        if check and result.returncode:
            raise Refusal(f"git {' '.join(args)} for {self.worktree}: {result.stderr.strip()}")
        return result

    def text(self, path):
        """Read a regular metadata file, refusing symlink substitution."""
        path = Path(path)
        if path.is_symlink() or not path.is_file():
            raise Refusal(f"missing or nonregular verifier metadata: {path}")
        return path.read_text().strip()

    def registration(self):
        """Find the unique administration whose backlink names this checkout."""
        matches = []
        root = self.common / "worktrees"
        if root.exists():
            for admin in root.iterdir():
                if admin.is_symlink():
                    raise Refusal(f"ambiguous symlink registration {admin}")
                backlink = admin / "gitdir"
                if backlink.is_file() and not backlink.is_symlink():
                    if Path(backlink.read_text().strip()) == self.worktree / ".git":
                        matches.append(admin)
        for admin in matches:
            if (admin / "commondir").is_symlink() or (admin / self.text(admin / "commondir")).resolve() != self.common:
                raise Refusal(f"common directory mismatch: {admin} versus {self.common}")
            if (admin / "locked").exists():
                raise Refusal(f"locked registration {admin}: {self.text(admin / 'locked')}")
        if len(matches) > 1:
            pointer = self.text(self.worktree / ".git")
            current = [admin for admin in matches if pointer == f"gitdir: {admin}"]
            if (not self.managed or len(matches) != 2 or len(current) != 1
                    or current[0] == self.canonical or self.canonical not in matches
                    or self.state.get("admin") != str(self.canonical)):
                raise Refusal(f"ambiguous registrations claim {self.worktree}: {matches}; pointer={pointer}")
            self.retain_stale(self.canonical)
            return current[0]
        return matches[0] if matches else None

    def retain_stale(self, admin):
        """Retain metadata proven to be a previous owned registration."""
        root = Path(tempfile.mkdtemp(prefix="verification-recovery-", dir=self.common / "storyhook"))
        self.state["stale"] = {"source": str(admin), "target": str(root / "stale-admin")}
        self.persist()
        self.finish_stale()

    def finish_stale(self):
        """Converge an interrupted stale-registration retention rename."""
        pending = self.state.get("stale")
        if not pending:
            return
        source, target = Path(pending["source"]), Path(pending["target"])
        if (source.parent != self.common / "worktrees" or target.parent.parent != self.common / "storyhook"
                or not target.parent.name.startswith("verification-recovery-") or target.name != "stale-admin"
                or source.is_symlink() or target.is_symlink()):
            raise Refusal(f"invalid stale registration intent in {self.path}")
        if source.exists() and target.exists():
            raise Refusal(f"stale retention collision: {source} -> {target}")
        actual = source if source.exists() else target
        if self.text(actual / "gitdir") != str(self.worktree / ".git"):
            raise Refusal(f"stale ownership changed at {actual}")
        if source.exists():
            source.rename(target)
            sync_dir(source.parent)
            sync_dir(target.parent)
        save(target.parent / "manifest.json", self.state)
        self.state.pop("stale")
        self.persist()

    def point(self, admin):
        """Publish a forward pointer only after establishing its owner."""
        atomic(self.worktree / ".git", f"gitdir: {admin}\n".encode())

    def normalize(self, admin):
        """Rename only owned metadata, preserving its HEAD/index/refs/reflogs."""
        if not self.managed or admin == self.canonical:
            return admin
        if self.canonical.exists() or self.canonical.is_symlink():
            backlink = self.canonical / "gitdir"
            mapping = backlink.read_text().strip() if backlink.is_file() else "<missing backlink>"
            raise Refusal(f"canonical name collision: {self.canonical} -> {mapping}; verifier {admin} -> {self.worktree / '.git'}; destination is not proven stale and owned")
        self.state["rename"] = {"source": str(admin), "target": str(self.canonical)}
        self.persist()
        self.finish_rename()
        return self.canonical

    def finish_rename(self):
        """Converge whether interruption preceded or followed the directory rename."""
        pending = self.state.get("rename")
        if not pending:
            return
        source, target = Path(pending["source"]), Path(pending["target"])
        if source.parent != self.common / "worktrees" or target != self.canonical:
            raise Refusal(f"invalid rename intent in {self.path}")
        if source.exists() and target.exists():
            raise Refusal(f"rename collision: both {source} and {target} exist; retain {self.path}")
        owner = source if source.exists() else target
        if owner.is_symlink() or self.text(owner / "gitdir") != str(self.worktree / ".git"):
            raise Refusal(f"rename owner mismatch: {owner} and {self.worktree}")
        link = self.text(self.worktree / ".git")
        if link not in (f"gitdir: {source}", f"gitdir: {target}"):
            raise Refusal(f"rename pointer conflict: {link}; {source} -> {target}")
        if source.exists():
            source.rename(target)
            sync_dir(target.parent)
        self.point(target)
        del self.state["rename"]
        self.persist()

    def check_lease(self, lease, admin):
        """Validate a recorded private lease and both original mappings."""
        if lease.parent != self.common / "storyhook" or not lease.name.startswith("merge-watch-objects.") or lease.is_symlink():
            raise Refusal(f"invalid owned lease path {lease}")
        if lease.resolve() != lease or (lease / ".git").is_symlink():
            raise Refusal(f"private administration is not physical: {lease}")
        if self.text(lease / "original-gitlink") != f"gitdir: {admin}":
            raise Refusal(f"lease {lease} original pointer does not name {admin}")
        if self.text(lease / ".git/gitdir") != str(self.worktree / ".git") or Path(self.text(lease / ".git/commondir")) != self.common:
            raise Refusal(f"private lease mappings conflict: {lease} versus {self.worktree} / {self.common}")

    def archive(self, admin, lease=None):
        """Journal retention of the whole diagnostic unit before replacing it."""
        recovery = Path(tempfile.mkdtemp(prefix="verification-recovery-", dir=self.common / "storyhook"))
        self.state["archive"] = {"root": str(recovery), "admin": str(admin),
                                 "lease": str(lease) if lease else None}
        self.persist()
        self.finish_archive()

    def finish_archive(self):
        """Resume an interrupted evidence move using recorded source identities."""
        pending = self.state.get("archive")
        if not pending:
            return
        root, admin = Path(pending["root"]), Path(pending["admin"])
        lease = Path(pending["lease"]) if pending["lease"] else None
        if root.parent != self.common / "storyhook" or not root.name.startswith("verification-recovery-") or root.is_symlink() or admin.parent != self.common / "worktrees":
            raise Refusal(f"invalid retention intent in {self.path}")
        moves = [(self.worktree, root / "worktree"), (admin, root / "admin")]
        if lease:
            if lease.parent != self.common / "storyhook" or not lease.name.startswith("merge-watch-objects."):
                raise Refusal(f"invalid retained lease in {self.path}")
            moves.append((lease, root / "lease"))
        for source, destination in moves:
            if source.is_symlink() or destination.is_symlink() or (source.exists() and destination.exists()):
                raise Refusal(f"retention collision: {source} -> {destination}; preserve {self.path}")
            if source.exists():
                source.rename(destination)
                sync_dir(source.parent)
                sync_dir(destination.parent)
            elif not destination.exists():
                raise Refusal(f"missing retention source and destination: {source} -> {destination}")
        retained = root / "worktree"
        atomic(root / "admin/gitdir", f"{retained / '.git'}\n".encode())
        # The archived admin left Git's shared worktrees directory; make its
        # common directory explicit so HEAD, index and reflogs remain readable.
        atomic(root / "admin/commondir", f"{self.common}\n".encode())
        target = root / "lease/.git" if lease else root / "admin"
        if lease:
            atomic(target / "gitdir", f"{retained / '.git'}\n".encode())
        atomic(retained / ".git", f"gitdir: {target}\n".encode())
        save(root / "manifest.json", self.state)
        atomic(root / "README.md", ("# Retained verifier evidence\n\n"
              f"Original checkout: {self.worktree}\nOriginal administration: {admin}\n"
              f"Common objects: {self.common / 'objects'}\n"
              "The complete checkout, index, HEAD and reflogs are retained. Pointer backlinks were relocated.\n"
              "For a retained private lease, run Git with GIT_OBJECT_DIRECTORY=<this directory>/lease/objects "
              "and GIT_ALTERNATE_OBJECT_DIRECTORIES=<common objects>, in worktree/.\n"
              "No file edits were reset; this is infrastructure evidence, not a gate receipt.\n").encode())
        print(f"verifier-worktree: preserved verifier evidence at {root}", file=sys.stderr)
        self.archived = True
        self.state.pop("archive")
        self.state.pop("lease", None)
        self.state.pop("restore", None)
        self.persist()

    def recover(self):
        """Restore recorded clean leases or preserve damaged ones as a unit."""
        self.finish_archive()
        self.finish_stale()
        self.finish_rename()
        lease_text = self.state.get("lease")
        if not lease_text:
            return
        lease = Path(lease_text)
        admin = self.registration()
        if admin is None:
            raise Refusal(f"private lease has no unique registered owner: {lease}")
        if self.state.get("lease_phase") == "building":
            if self.text(self.worktree / ".git") != f"gitdir: {admin}":
                raise Refusal(f"unpublished lease has unexpected active pointer: {lease}")
            if lease.parent != self.common / "storyhook" or not lease.name.startswith("merge-watch-objects.") or lease.is_symlink():
                raise Refusal(f"invalid unpublished lease: {lease}")
            if lease.exists():
                shutil.rmtree(lease)
                sync_dir(lease.parent)
            self.state.pop("lease")
            self.state.pop("lease_phase")
            self.persist()
            return
        # Deletion can stop halfway through a directory. Only a durable linked
        # phase plus the restored shared checkout permits resuming that deletion.
        if self.state.get("restore") == "linked":
            if self.text(self.worktree / ".git") != f"gitdir: {admin}":
                raise Refusal(f"removed lease has inconsistent pointer at {self.worktree}")
            self.git("diff", "--quiet")
            self.git("diff", "--cached", "--quiet", self.state["base"])
            if lease.parent != self.common / "storyhook" or not lease.name.startswith("merge-watch-objects.") or lease.is_symlink():
                raise Refusal(f"invalid cleanup lease {lease}")
            if lease.exists():
                shutil.rmtree(lease)
                sync_dir(lease.parent)
            self.state.pop("lease")
            self.state.pop("restore")
            self.persist()
            return
        self.check_lease(lease, admin)
        pointer = self.text(self.worktree / ".git")
        if pointer not in (f"gitdir: {admin}", f"gitdir: {lease / '.git'}"):
            raise Refusal(f"private pointer conflict: {pointer}; registered={admin}; lease={lease}")
        clean = (self.git("diff", "--quiet", private=lease, check=False).returncode == 0
                 and self.git("diff", "--cached", "--quiet", private=lease, check=False).returncode == 0)
        if not clean:
            self.archive(admin, lease)
            return
        self.state["restore"] = "checkout"
        self.persist()
        checkout = self.git("checkout", "-q", "--detach", self.state["base"], private=lease, check=False)
        if checkout.returncode:
            self.state["recovery_reason"] = checkout.stderr.strip()
            self.persist()
            self.archive(admin, lease)
            return
        self.point(admin)
        self.state["restore"] = "linked"
        self.persist()
        # Use the same checked deletion path on ordinary exit and restart.
        self.recover()

    def tracked_clean(self):
        """Include index changes and unreadability when judging retained evidence."""
        return (self.git("diff", "--quiet", "--no-ext-diff", check=False).returncode == 0
                and self.git("diff", "--cached", "--quiet", "--no-ext-diff", "HEAD", "--",
                             check=False).returncode == 0)

    def startup_damage(self, admin):
        """Unfinished Git operations belong to the retained diagnostic unit."""
        pending = ("index.lock", "HEAD.lock", "MERGE_HEAD", "AUTO_MERGE",
                   "CHERRY_PICK_HEAD", "REVERT_HEAD", "BISECT_START",
                   "rebase-merge", "rebase-apply", "sequencer")
        return not self.tracked_clean() or any(
            (admin / name).exists() or (admin / name).is_symlink() for name in pending)

    def clean_startup(self, base):
        """Remove disposable inputs only after owned damage has been retained."""
        # Intent survives a partial clean. Every ensure revalidates ownership,
        # mappings and tracked evidence before it can resume these deletions.
        self.state["startup_cleanup"] = {"base": base}
        self.persist()
        cleaned = self.git("clean", "-ffdx")
        if cleaned.stdout:
            print(f"verifier-worktree: removed disposable leftovers at {self.worktree}",
                  file=sys.stderr)
        self.git("checkout", "-q", "--detach", base)
        if (not self.tracked_clean()
                or self.git("clean", "-nffdx").stdout
                or self.git("rev-parse", "HEAD").stdout.strip() != base
                or self.git("symbolic-ref", "-q", "HEAD", check=False).returncode != 1):
            raise Refusal(f"startup cleanup did not establish a clean detached verifier at {base}: {self.worktree}; retained {self.path}")
        self.state.pop("startup_cleanup")
        self.persist()

    def ensure(self, base, replaced=False):
        """Establish a stable, canonical and ordinarily resolvable verifier."""
        self.recover()
        base = self.git("rev-parse", "--verify", f"{base}^{{commit}}").stdout.strip()
        admin = self.registration()
        if admin:
            if not self.worktree.exists():
                # Missing checkout is stale only for this exact owned backlink;
                # retain metadata instead of pruning unrelated worktrees.
                self.retain_stale(admin)
                admin = None
            else:
                link = self.text(self.worktree / ".git")
                if link != f"gitdir: {admin}":
                    raise Refusal(f"orphaned private or corrupt pointer {self.worktree / '.git'} -> {link}; registered {admin} -> {self.worktree / '.git'}; no owned lease journal, refusing ambiguous legacy recovery")
                admin = self.normalize(admin)
        if admin is None:
            if self.worktree.exists():
                raise Refusal(f"unregistered verifier path {self.worktree}; preserve unclassified evidence")
            if self.managed and self.canonical.exists():
                mapping = self.text(self.canonical / "gitdir")
                raise Refusal(f"canonical name collision: {self.canonical} -> {mapping}; expected {self.worktree / '.git'}")
            self.worktree.parent.mkdir(parents=True, exist_ok=True)
            self.git("worktree", "add", "-q", "--detach", str(self.worktree), base)
            admin = self.registration()
            if admin is None:
                raise Refusal(f"new verifier has no registration: {self.worktree}")
            admin = self.normalize(admin)
        healthy = self.git("cat-file", "-e", "HEAD^{commit}", check=False).returncode == 0
        if healthy:
            reflog = self.git("reflog", "show", "--format=%H", "HEAD", check=False)
            healthy = reflog.returncode == 0 and all(
                self.git("cat-file", "-e", oid, check=False).returncode == 0
                for oid in reflog.stdout.splitlines())
        if not healthy or (self.managed and self.startup_damage(admin)):
            if replaced:
                raise Refusal(f"replacement verifier is still damaged at {self.worktree}; preserve {self.path} and inspect checkout hooks")
            self.archive(admin)
            return self.ensure(base, replaced=True)
        # A broken foreign ref is an actionable refusal, never justification
        # for repeatedly rebuilding this otherwise healthy verifier.
        self.git("rev-list", "--objects", "--all", "--reflog")
        if self.git("rev-parse", "--git-common-dir").stdout.strip() != str(self.common):
            # Git can return a relative commondir; resolve using this checkout.
            value = self.git("rev-parse", "--git-common-dir").stdout.strip()
            if (self.worktree / value).resolve() != self.common:
                raise Refusal(f"resolved common directory mismatch: {value}")
        if self.managed:
            self.clean_startup(base)
            atomic(self.common / "storyhook/verification-worktree.format", b"private-gitdir-v1\n")
        self.state["admin"] = str(admin)
        self.persist()

    def allocate(self, base):
        """Persist ownership before creating the first private temporary file."""
        if self.state.get("lease"):
            raise Refusal(f"unrecovered previous lease in {self.path}")
        admin = self.registration()
        if admin is None or self.text(self.worktree / ".git") != f"gitdir: {admin}":
            raise Refusal(f"cannot allocate from inconsistent verifier {self.worktree}")
        lease = self.common / "storyhook" / ("merge-watch-objects." + uuid.uuid4().hex)
        self.state.update(lease=str(lease), base=base, admin=str(admin), lease_phase="building")
        self.persist()
        lease.mkdir()
        sync_dir(lease.parent)
        print(lease)

    def register(self, lease, base):
        """Record complete lease identity before the speculative pointer swap."""
        admin = self.registration()
        if admin is None or self.text(self.worktree / ".git") != f"gitdir: {admin}":
            raise Refusal(f"cannot lease inconsistent verifier {self.worktree}")
        self.check_lease(lease, admin)
        if self.state.get("lease") != str(lease) or self.state.get("lease_phase") != "building":
            raise Refusal(f"lease was not allocated by this lifecycle: {lease}")
        self.state.update(lease=str(lease), base=base, admin=str(admin), lease_phase="ready")
        self.persist()


def main():
    """Expose only internal operations that require a matching live owner."""
    try:
        mode, common_arg, worktree_arg = sys.argv[1:4]
        common, worktree, key = paths(common_arg, worktree_arg)
        if not held(common, worktree, key):
            raise Refusal(f"no live lifecycle ownership for {worktree}")
        owner = read(str(key) + ".owner")
        if owner.get("gate_started"):
            raise Refusal(f"gate execution has not established quiescence; preserve {worktree} and {key}.owner")
        workspace = Workspace(common, worktree, key)
        if mode == "ensure":
            workspace.ensure(sys.argv[4])
        elif mode == "allocate":
            workspace.allocate(sys.argv[4])
        elif mode == "register":
            workspace.register(Path(sys.argv[4]), sys.argv[5])
        elif mode == "recover":
            workspace.recover()
            if workspace.archived:
                return 2
        else:
            raise Refusal(f"unknown lifecycle operation {mode}")
        return 0
    except (Refusal, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"verifier-worktree: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
