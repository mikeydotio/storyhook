"""Pin the external archive used by the real plugin transport tests."""
import ast
from pathlib import Path
import re
import sys
from verifier_state import Refusal


def revivify_fixture(worktree, env, query):
    """Hash the exact archived test revision; never use the provider's live HEAD."""
    selector = env.get('STORY_TEST_REVIVIFY_REPO')
    if not selector:
        raise Refusal('measurement requires the explicit revivify test repository')
    repository = Path(selector).resolve(strict=True)
    if not repository.is_dir():
        raise Refusal('revivify test repository is not a directory')
    source = Path(worktree) / 'plugins/story/tests/test_revivify_transport.py'
    revisions = [node.value.value for node in ast.parse(source.read_text()).body
                 if isinstance(node, ast.Assign) and len(node.targets) == 1
                 and isinstance(node.targets[0], ast.Name) and node.targets[0].id == 'REVISION'
                 and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str)]
    if len(revisions) != 1 or not re.fullmatch(r'[a-f0-9]{40}', revisions[0]):
        raise Refusal('transport fixture must declare one exact revision')
    revision = revisions[0]
    tree = query(['git', '-C', str(repository), 'rev-parse', '--verify', revision + '^{tree}'])
    if not re.fullmatch(r'[a-f0-9]{40}', tree):
        raise Refusal('revivify fixture tree identity is invalid')
    # Positional arguments keep paths out of shell source. pipefail retains a
    # missing object/archive failure even when the digest process exits zero.
    command = ['bash', '-c', 'set -o pipefail; git -C "$1" archive --format=tar "$2" | '
               '"$3" -B -c \'import hashlib,sys; print(hashlib.file_digest(sys.stdin.buffer,"sha256").hexdigest())\'',
               'revivify-fixture', str(repository), revision, sys.executable]
    archive = query(command)
    if not re.fullmatch(r'[a-f0-9]{64}', archive):
        raise Refusal('revivify fixture archive digest is invalid')
    return {'repository': str(repository), 'revision': revision, 'tree': tree,
            'archive_sha256': archive}
