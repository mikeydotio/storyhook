"""Conservative byte identities for measurement inputs, without copying secrets.

All configured dependency roots are read as data and hashed; no discovery text is
executed. Missing optional config files remain part of the identity. Unknown file
types, symlinks inside roots, concurrent writes or a deadline refuse measurement.
"""

from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import re
import shutil
import stat
import sys
import sysconfig
import tomllib

from gate_measurement_cohorts import canonical, fingerprint
from gate_measurement_bounds import Deadline
from gate_measurement_runtime import capture, resource_limits
from gate_measurement_storage import directory_identity
from verifier_state import Refusal

# These are independently represented source/target locations or lifecycle-only
# telemetry. All other environment variables, including unknown ones, are hashed.
CONTEXT_ENV = {
    'PWD', 'OLDPWD', '_', 'CARGO_TARGET_DIR', 'STORYHOOK_GATE_MEASUREMENT',
    'STORYHOOK_GATE_PROGRESS', 'STORYHOOK_GATE_EXECUTION_FILE',
    'STORYHOOK_MEASUREMENT_SLOT', 'STORYHOOK_MEASUREMENT_GATE_DEADLINE',
    'STORYHOOK_MEASUREMENT_END', 'STORYHOOK_MEASUREMENT_OPERATIONS',
    'STORYHOOK_VERIFIER_OWNER',
}
WORKERS = ('CARGO_BUILD_JOBS', 'STORYHOOK_TEST_THREAD_BUDGET',
           'STORYHOOK_PLUGIN_JOBS', 'STORYHOOK_E2E_JOBS')


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def stamp(path):
    try:
        value = Path(path).lstat()
    except FileNotFoundError:
        return None
    return (value.st_dev, value.st_ino, value.st_mode, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns)


def ca_source(path, target):
    """Require the one physical, versioned certifi layout for the public bundle."""
    path, target = Path(path), Path(target)
    prefix = target.parent.parent.parent
    try:
        parts = path.relative_to(prefix / 'Cellar').parts
        valid = (target == prefix / 'etc/ca-certificates/cert.pem'
                 and len(parts) == 7 and parts[0] == 'certifi'
                 and re.fullmatch(r'[0-9][a-zA-Z0-9._+-]*', parts[1])
                 and parts[2] == 'lib' and re.fullmatch(r'python[0-9]+\.[0-9]+', parts[3])
                 and parts[4:] == ('site-packages', 'certifi', 'cacert.pem')
                 and path.is_absolute() and path.parent.resolve(strict=True) == path.parent
                 and path.is_symlink() and path.resolve(strict=True) == target)
    except (ValueError, OSError, RuntimeError):
        valid = False
    if not valid:
        raise Refusal('public CA bundle requires its physical versioned certifi source')


def ca_fields(value):
    """Retain file identity, content-change metadata and the single-link invariant."""
    return [value.st_dev, value.st_ino, value.st_mode, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns, value.st_nlink]


@contextmanager
def open_ca_bundle(path):
    """Pin each ancestor descriptor without following links, including races."""
    descriptors = []
    try:
        path = Path(path)
        if not path.is_absolute() or '..' in path.parts:
            raise Refusal('public CA bundle path must be absolute and physical')
        fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptors.append(fd)
        ancestors = []
        for component in path.parts[1:-1]:
            value = os.fstat(fd)
            ancestors.append([value.st_dev, value.st_ino, value.st_mode])
            fd = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            descriptors.append(fd)
        value = os.fstat(fd)
        ancestors.append([value.st_dev, value.st_ino, value.st_mode])
        before = os.stat(path.name, dir_fd=fd, follow_symlinks=False)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise Refusal('public CA bundle must be a regular file with one link')
        leaf = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
        descriptors.append(leaf)
        value = os.fstat(leaf)
        if not stat.S_ISREG(value.st_mode) or value.st_nlink != 1:
            raise Refusal('public CA bundle must be a regular file with one link')
        if ca_fields(before) != ca_fields(value):
            raise Refusal('public CA bundle changed while opening')
        yield leaf, [ancestors, ca_fields(value)]
    except OSError as error:
        raise Refusal('public CA bundle cannot be opened without following links') from error
    finally:
        for fd in reversed(descriptors):
            os.close(fd)


def snapshot_ca_bundle(path, declaration, deadline, audit):
    """Hash one declared public bundle through pinned, validated file descriptors."""
    if (declaration.get('kind') != 'public-ca-bundle'
            or not declaration.get('sources')):
        raise Refusal('public CA bundle declaration is incomplete')
    for source in declaration['sources']:
        ca_source(source, path)
    with open_ca_bundle(path) as (fd, before):
        expected = {'public-ca-bundle': before}
        if path in audit and audit[path] != expected:
            raise Refusal('public CA bundle changed between inventory references')
        if path not in audit and len(audit) >= 250000:
            raise Refusal('input inventory exceeded its 250000-entry memory bound')
        audit[path] = expected
        h = hashlib.sha256()
        while True:
            deadline.require('public CA bundle hashing')
            block = os.read(fd, 1024 * 1024)
            if not block:
                break
            h.update(block)
        if ca_fields(os.fstat(fd)) != before[1]:
            raise Refusal('public CA bundle changed while hashing')
        with open_ca_bundle(path) as (_, after):
            if before != after:
                raise Refusal('public CA bundle ancestry or file changed while hashing')
    return {'kind': 'file', 'sha256': h.hexdigest(), 'mode': stat.S_IMODE(before[1][2])}


def check_audit(audit, deadline):
    for path, expected in audit.items():
        deadline.require('measurement input stability check')
        if isinstance(expected, dict) and 'public-ca-bundle' in expected:
            with open_ca_bundle(path) as (_, current):
                if current != expected['public-ca-bundle']:
                    raise Refusal('public CA bundle changed during complete inventory capture')
            continue
        if stamp(path) != expected:
            raise Refusal('input changed during the complete inventory capture')


def snapshot(path, deadline, *, allowed=(), trail=(), external_files=None):
    audit = {}
    result = _snapshot(path, deadline, allowed=allowed, trail=trail, audit=audit, external_files=external_files)
    check_audit(audit, deadline)
    return result


def _snapshot(path, deadline, *, allowed, trail=(), audit, external_files=None):
    """Hash exact regular-file bytes and modes; never follow a substituted link."""
    path = Path(path)
    deadline.require('measurement input capture')
    external_files = external_files or {}
    if str(path) in external_files:
        return snapshot_ca_bundle(path, external_files[str(path)], deadline, audit)
    if not path.is_absolute() or path.parent.resolve() != path.parent:
        raise Refusal('input path is not physical and absolute')
    observed_stamp = stamp(path)
    if path not in audit and len(audit) >= 250000:
        raise Refusal('input inventory exceeded its 250000-entry memory bound')
    if path in audit and audit[path] != observed_stamp:
        raise Refusal('input changed between references in the same inventory')
    audit[path] = observed_stamp
    try:
        before = path.lstat()
    except FileNotFoundError:
        return {'kind': 'absent'}
    if stat.S_ISLNK(before.st_mode):
        for declared_path, declaration in external_files.items():
            if str(path) in declaration['sources']:
                ca_source(path, declared_path)
        try:
            target = path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise Refusal(f'input symlink cannot be resolved: {path}') from error
        if str(target) in external_files:
            ca_source(path, target)
            if str(path) not in external_files[str(target)]['sources']:
                raise Refusal('public CA bundle source was not declared')
        if not any(target == root or root in target.parents for root in allowed):
            raise Refusal(f'input symlink escapes its declared dependencies: {path} -> {target}')
        link = os.readlink(path)
        if target in trail:
            # SDK header aliases can point back into an ancestor directory.
            # That directory's entire contents are already being captured by
            # this walk. Record the edge, not an infinitely repeated subtree;
            # the shared audit still verifies its identity and all file bytes.
            if target not in audit or not stat.S_ISDIR(target.lstat().st_mode):
                raise Refusal('input reference lacks an observed ancestor directory')
            value = {'kind': 'ancestor-directory-reference', 'path': str(target)}
        else:
            value = _snapshot(target, deadline, allowed=allowed, trail=(*trail, path), audit=audit, external_files=external_files)
        after = path.lstat()
        if (before.st_dev, before.st_ino, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_ctime_ns) or os.readlink(path) != link:
            raise Refusal('input symlink changed during observation')
        return {'kind': 'symlink', 'link': link, 'target': value}
    if stat.S_ISREG(before.st_mode):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(fd)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise Refusal('input file changed while opening')
            h = hashlib.sha256()
            while True:
                deadline.require('measurement input hashing')
                block = os.read(fd, 1024 * 1024)
                if not block:
                    break
                h.update(block)
            after = os.fstat(fd)
        finally:
            os.close(fd)
        fields = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns, s.st_mode)
        if fields(before) != fields(after) or fields(path.lstat()) != fields(after):
            raise Refusal('input file changed while hashing')
        return {'kind': 'file', 'sha256': h.hexdigest(), 'mode': stat.S_IMODE(after.st_mode)}
    if not stat.S_ISDIR(before.st_mode):
        raise Refusal('input contains a symlink or nonregular entry: ' + str(path))
    names = sorted(os.listdir(path))
    children = {name: _snapshot(path / name, deadline, allowed=allowed, trail=(*trail, path), audit=audit, external_files=external_files) for name in names}
    after = path.lstat()
    if ((before.st_dev, before.st_ino, before.st_mtime_ns, before.st_ctime_ns)
            != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_ctime_ns)
            or sorted(os.listdir(path)) != names):
        raise Refusal('input directory changed while hashing')
    return {'kind': 'directory', 'sha256': digest(children), 'mode': stat.S_IMODE(after.st_mode)}


def cargo_config_paths(worktree, env):
    """Cargo reads both names in each ancestor and in its explicit home."""
    home = env.get('CARGO_HOME') or str(Path(env['HOME']) / '.cargo')
    paths = [Path(home) / name for name in ('config', 'config.toml')]
    for ancestor in (Path(worktree), *Path(worktree).parents):
        paths.extend(ancestor / '.cargo' / name for name in ('config', 'config.toml'))
    return sorted(set(paths))


def git_config_origins(raw, worktree):
    """Names-only Git discovery includes active include/includeIf files.

    Quoted/non-file origins are deliberately unsupported instead of guessing at
    Git's filename escaping. No configuration value is requested or persisted.
    """
    result = set()
    for line in raw.splitlines():
        fields = line.split('\t')
        if (len(fields) != 2 or not fields[0].startswith('file:')
                or '"' in fields[0] or not fields[1]):
            raise Refusal('Git configuration origin is unsupported or unreadable')
        path = Path(fields[0][5:])
        result.add(str((Path(worktree) / path).resolve(strict=True)))
    return sorted(result)


def supported_compiler_profile(config_paths, worktree, env, query):
    """Do not claim closure over an arbitrary external compiler/runner program."""
    if env.get('RUSTFLAGS') or env.get('RUSTDOCFLAGS'):
        raise Refusal('custom compiler flags require an explicit reviewed input inventory')
    worktree = Path(worktree)
    for path in config_paths:
        if not Path(path).exists():
            continue
        with open(path, 'rb') as stream:
            config = tomllib.load(stream)
        if config.get('include') or config.get('env'):
            raise Refusal('Cargo include/env overrides need explicit dependency review')
        aliases = config.get('alias', {})
        if any(name in aliases for name in ('fmt', 'clippy', 'build', 'test', 'fetch', 'metadata')):
            raise Refusal('Cargo gate command aliases are not part of the pinned standard profile')
        tables = [config.get('build', {}), *config.get('target', {}).values()]
        for table in tables:
            if table.get('rustflags') or table.get('rustdocflags'):
                raise Refusal('configured compiler flags need explicit dependency review')
            for name in ('rustc', 'rustc-wrapper', 'rustc-workspace-wrapper', 'linker', 'runner'):
                command = table.get(name)
                if not command:
                    continue
                executable = command[0] if isinstance(command, list) else command
                if not isinstance(executable, str) or not executable:
                    raise Refusal('configured tool command is malformed')
                resolved = (worktree / executable).resolve(strict=True)
                if worktree not in resolved.parents:
                    raise Refusal('external configured compiler/runner needs dependency review')
                relative = str(resolved.relative_to(worktree))
                if query(['git', 'ls-files', '--error-unmatch', '--', relative]).strip() != relative:
                    raise Refusal('configured compiler/runner is not tracked by the pinned source')


def rust_linked_components(sysroot):
    """Declare Homebrew's selected LLVM component package, not arbitrary links.

    Homebrew Rust ships rust-objcopy as a link into its versioned LLVM package.
    Include that whole package so its tools and libraries are input bytes too.
    Other escaping links remain unsupported; discovery does not bless them.
    """
    sysroot = Path(sysroot)
    result = {}
    for component in sorted(sysroot.glob('lib/rustlib/*/bin/rust-objcopy')):
        if not component.is_symlink():
            continue
        target = component.resolve(strict=True)
        if sysroot in target.parents:
            continue
        cellar = sysroot.parent.parent
        if cellar.name != 'Cellar' or cellar not in target.parents:
            raise Refusal('external Rust component requires a reviewed package inventory')
        relative = target.relative_to(cellar).parts
        if (len(relative) != 4 or not re.fullmatch(r'llvm(?:@[0-9]+)?', relative[0])
                or relative[2:] != ('bin', 'llvm-objcopy') or not target.is_file()):
            raise Refusal('external Rust component is not the supported LLVM package')
        package = cellar / relative[0] / relative[1]
        result['rust-linked-llvm-' + digest(str(package))] = str(package)
        configuration = package / 'etc/clang'
        if configuration.is_symlink():
            target_config = configuration.resolve(strict=True)
            if target_config != cellar.parent / 'etc/clang':
                raise Refusal('LLVM configuration link is outside its supported Homebrew prefix')
            result['rust-linked-llvm-clang-config'] = str(target_config)
    return result


def python_distribution(prefix):
    """Homebrew's framework links into files in the same selected package."""
    prefix = Path(prefix).resolve(strict=True)
    for candidate in (prefix, *prefix.parents):
        if (candidate.parent.parent.name == 'Cellar'
                and re.fullmatch(r'python(?:@[0-9.]+)?', candidate.parent.name)):
            return candidate
    return prefix


def python_linked_packages(runtime, libraries, *, deadline=None, entry_limit=250000, external_files=None):
    """Close installed Homebrew Python links over versioned Cellar packages.

    Linked files are not enough: include the complete selected package and walk
    its links too. Only packages in this interpreter's physical Cellar can be
    added, plus certifi's exact regular public CA bundle in that prefix. Its
    typed file declaration follows all recursive reads and the final audit.
    Other external configuration, prefixes and unknown package layouts refuse.
    """
    deadline = deadline or Deadline(30)
    external_files = external_files if external_files is not None else {}
    runtime = Path(runtime).resolve(strict=True)
    cellar = runtime.parent.parent
    if cellar.name != 'Cellar':
        return {}  # Non-Homebrew escaping links still refuse during capture.
    roots = {runtime, *(Path(p).resolve(strict=True) for p in libraries)}
    pending = list(sorted(roots))
    visited = set()
    result = {}
    while pending:
        path = pending.pop()
        deadline.require('Python dependency link discovery')
        if path in visited:
            continue
        if len(visited) >= entry_limit:
            raise Refusal('Python dependency discovery exceeded its entry bound')
        visited.add(path)
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            try:
                target = path.resolve(strict=True)
            except (OSError, RuntimeError) as error:
                raise Refusal(f'Python dependency link cannot be resolved: {path}') from error
            bundle = cellar.parent / 'etc/ca-certificates/cert.pem'
            nominal_target = Path(os.path.abspath(path.parent / os.readlink(path)))
            if target == bundle or nominal_target == bundle:
                ca_source(path, bundle)
                with open_ca_bundle(target):
                    pass
                declaration = external_files.setdefault(str(target), {'kind': 'public-ca-bundle', 'sources': []})
                declaration['sources'] = sorted(set([*declaration['sources'], str(path)]))
                result['python-public-ca-bundle'] = str(target)
                continue
            if any(target == root or root in target.parents for root in roots):
                continue
            if cellar not in target.parents:
                raise Refusal(f'Python dependency link leaves its installed Cellar: {path}')
            parts = target.relative_to(cellar).parts
            if (len(parts) < 2 or not re.fullmatch(r'[a-z0-9][a-z0-9+_.@-]*', parts[0])
                    or not re.fullmatch(r'[0-9][a-zA-Z0-9._+-]*', parts[1])):
                raise Refusal('Python dependency link lacks an exact installed package')
            package = cellar / parts[0] / parts[1]
            if package.resolve(strict=True) != package or not package.is_dir():
                raise Refusal('Python dependency package identity is not physical')
            roots.add(package)
            result['python-linked-package-' + digest(str(package))] = str(package)
            pending.append(package)
        elif stat.S_ISDIR(mode):
            with os.scandir(path) as entries:
                for item in entries:
                    if len(visited) + len(pending) >= entry_limit:
                        raise Refusal('Python dependency discovery exceeded its entry bound')
                    pending.append(Path(item.path))
        elif not stat.S_ISREG(mode):
            raise Refusal(f'Python dependency contains a nonregular input: {path}')
    return dict(sorted(result.items()))


def inventory(worktree, env, *, query=capture):
    """Resolve mandatory tools and mutable dependency roots before pinning.

    Config files containing executable credential-provider or source replacement
    directives are still fingerprinted, but no credentials file is opened. The
    actual campaign must be offline: fetching/replacing dependencies during a
    measured run would invalidate the before/after snapshots.
    """
    home = Path(env.get('CARGO_HOME') or str(Path(env['HOME']) / '.cargo')).resolve()
    roots = {'cargo-registry-source': str(home / 'registry' / 'src'),
             'cargo-registry-index': str(home / 'registry' / 'index'),
             'cargo-git-checkouts': str(home / 'git' / 'checkouts'),
             'cargo-git-databases': str(home / 'git' / 'db')}
    tools = {}
    for name in ('rustc', 'cargo', 'git', 'make', 'node', 'bash', 'sh', 'cc', 'ar'):
        found = shutil.which(name, path=env.get('PATH'))
        if not found:
            raise Refusal('measurement tool is unavailable: ' + name)
        tools[name] = str(Path(found).resolve(strict=True))
    tools['python'] = str(Path(sys.executable).resolve(strict=True))
    # rustup's proxy binary alone does not identify the actual selected compiler.
    sysroot = Path(query(['rustc', '--print', 'sysroot'])).resolve(strict=True)
    roots['rust-sysroot'] = str(sysroot)
    roots.update(rust_linked_components(sysroot))
    sdk = Path(query(['xcrun', '--show-sdk-path'])).resolve(strict=True)
    roots['apple-sdk'] = str(sdk)
    clang = Path(query(['xcrun', '--find', 'clang'])).resolve(strict=True)
    roots['apple-toolchain'] = str(clang.parent.parent)
    roots['python-standard-library'] = str(Path(sysconfig.get_path('stdlib')).resolve(strict=True))
    roots['python-runtime'] = str(python_distribution(sys.base_prefix))
    for kind in ('purelib', 'platlib'):
        roots['python-' + kind] = str(Path(sysconfig.get_path(kind)).resolve(strict=True))
    external_files = {}
    roots.update(python_linked_packages(roots['python-runtime'],
                 [roots['python-standard-library'], roots['python-purelib'], roots['python-platlib']],
                 external_files=external_files))
    tools['selected-clang'] = str(clang)
    for name in ('xcrun', 'xcodebuild'):
        found = shutil.which(name, path=env.get('PATH'))
        if not found:
            raise Refusal('measurement Apple tool is unavailable: ' + name)
        tools[name] = str(Path(found).resolve(strict=True))
    # Node modules are mutable, ignored dependency input, independently of locks.
    roots['node-modules'] = str(Path(worktree) / 'node_modules')
    roots['browser-node-modules'] = str(Path(worktree) / 'e2e' / 'node_modules')
    if env.get('SDKROOT'):
        roots['sdkroot-override'] = str(Path(env['SDKROOT']).resolve(strict=True))
    import json
    metadata = json.loads(query(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1']))
    for package in metadata['packages']:
        package_root = Path(package['manifest_path']).resolve(strict=True).parent
        if package_root == Path(worktree) or Path(worktree) in package_root.parents:
            continue  # complete tracked workspace source has its own Git identity
        if any(package_root == Path(root) or Path(root) in package_root.parents for root in roots.values()):
            continue
        roots['external-package-' + digest(str(package_root))] = str(package_root)
    common = Path(query(['git', 'rev-parse', '--git-common-dir']))
    common = (Path(worktree) / common).resolve(strict=True)
    private = Path(query(['git', 'rev-parse', '--absolute-git-dir'])).resolve(strict=True)
    configs = cargo_config_paths(worktree, env)
    supported_compiler_profile(configs, worktree, env, query)
    configs.extend([Path(env['HOME']) / '.gitconfig', Path(env['HOME']) / '.config/git/config',
                    common / 'config', private / 'config.worktree'])
    origins = git_config_origins(query(['git', 'config', '--show-origin', '--name-only', '--list']), worktree)
    configs.extend(Path(p) for p in origins if Path(p) not in configs)
    return {'version': 1, 'tools': tools, 'dependency_roots': roots, 'external_files': external_files,
            'cargo_configs': [str(p) for p in configs]}


def observe(manifest, inventory_record, env, target, deadline, *, validate_source,
            versions, resolve_inventory, limits=resource_limits):
    """Capture every declared input on each boundary; no mtime digest cache."""
    validate_source()
    if inventory_record.get('version') != 1:
        raise Refusal('measurement input inventory is missing')
    if resolve_inventory() != inventory_record:
        raise Refusal('selected toolchain or dependency locations changed')
    allowed = tuple(Path(p) for p in [*inventory_record['tools'].values(),
                                     *inventory_record['dependency_roots'].values(),
                                     *inventory_record['cargo_configs']])
    audit = {}
    external_files = inventory_record.get('external_files', {})
    tools = {name: _snapshot(path, deadline, allowed=allowed, audit=audit, external_files=external_files) for name, path in inventory_record['tools'].items()}
    if not tools or any(row['kind'] != 'file' for row in tools.values()):
        raise Refusal('measurement executable inventory is incomplete')
    dependencies = {name: _snapshot(path, deadline, allowed=allowed, audit=audit, external_files=external_files)
                    for name, path in inventory_record['dependency_roots'].items()}
    configs = [_snapshot(path, deadline, allowed=allowed, audit=audit, external_files=external_files) for path in inventory_record['cargo_configs']]
    workers = {}
    for name in WORKERS:
        raw = env.get(name)
        if not isinstance(raw, str) or not raw.isdecimal() or not 1 <= int(raw) <= 8:
            raise Refusal('measurement requires an explicit bounded worker limit: ' + name)
        workers[name] = int(raw)
    workers['resource_limits'] = limits()
    actual_target = directory_identity(env.get('CARGO_TARGET_DIR', ''))
    if actual_target != target:
        raise Refusal('measurement target identity changed')
    result = {
        'source_commit': manifest['commit'], 'source_tree': manifest['tree'],
        'toolchain': {'executables': tools, 'versions': versions(), 'dependencies': dependencies},
        'environment_digest': digest({k: v for k, v in env.items() if k not in CONTEXT_ENV}),
        'configuration_digest': digest({'external_cargo_configs': configs, 'gate_argv': manifest['gate']['argv'],
                                        'project_configuration': manifest['gate'].get('configuration')}),
        'worker_limits': workers, 'gate_argv': manifest['gate']['argv'],
        'target_identity': target, 'applicable_legs': manifest['applicable_legs'],
    }
    fingerprint(result)
    check_audit(audit, deadline)
    if resolve_inventory() != inventory_record:
        raise Refusal('selected dependency locations changed during input capture')
    validate_source()
    return result
