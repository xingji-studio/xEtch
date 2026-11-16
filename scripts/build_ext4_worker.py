#!/usr/bin/env python3
"""Build the opt-in BSD lwext4 P0 worker; explicitly exclude GPL source files."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inventory():
    manifest = json.loads((ROOT / 'vendor/lwext4-origin.json').read_text())
    source = ROOT / 'vendor/lwext4'
    approved = {item['path']: item for item in manifest['files']}
    actual = {p.relative_to(source).as_posix() for p in source.rglob('*') if p.suffix in ('.c', '.h')}
    if actual != set(approved):
        raise RuntimeError(f'lwext4 source inventory changed: {actual ^ set(approved)}')
    for name, item in approved.items():
        path = source / name
        if digest(path) != item.get('vendored_sha256', item['sha256']):
            raise RuntimeError(f'Unreviewed lwext4 source change: {name}')
        if b'GNU General Public License' in path.read_bytes():
            raise RuntimeError(f'Disallowed source notice: {name}')
    if any((source / name).exists() for name in manifest['excluded_gpl_sources']):
        raise RuntimeError('Excluded GPL implementation present')
    return manifest, [source / name for name in sorted(approved) if name.endswith('.c')]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', choices=['native', 'windows'], default='native')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    manifest, sources = inventory()
    own = ROOT / 'native/ext4-worker'
    sources += [own / name for name in ('extents.c', 'xattr_disabled.c', 'worker.c')]
    windows = args.target == 'windows'
    compiler = 'x86_64-w64-mingw32-gcc' if windows else os.environ.get('CC', 'cc')
    output = (args.output or ROOT / 'target' / ('windows/release' if windows else 'release')).resolve()
    output.mkdir(parents=True, exist_ok=True)
    binary = output / ('xetch-ext4-worker.exe' if windows else 'xetch-ext4-worker')
    build = ROOT / 'artifacts' / ('lwext4-windows' if windows else 'lwext4-linux')
    build.mkdir(parents=True, exist_ok=True)
    flags = ['-std=c11', '-O2', '-g', '-D_FILE_OFFSET_BITS=64', '-DCONFIG_USE_DEFAULT_CFG=1',
             '-DCONFIG_DEBUG_PRINTF=0', '-DCONFIG_DEBUG_ASSERT=0', '-DCONFIG_HAVE_OWN_OFLAGS=1',
             '-DCONFIG_HAVE_OWN_ERRNO=0', '-DCONFIG_BLOCK_DEV_CACHE_SIZE=64',
             '-DCONFIG_EXTENTS_ENABLE=1', '-DCONFIG_XATTR_ENABLE=0',
             '-I' + str(ROOT / 'vendor/lwext4/include')]
    if windows:
        flags += ['-static']
    command = [compiler, *flags, *map(str, sources), '-o', str(binary)]
    with (build / 'build.log').open('wb') as log:
        result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError((build / 'build.log').read_text(errors='replace')[-10000:])
    profile = {'schema_version': 1, 'engine': 'lwext4', 'profile': 'bsd-extents-p0',
               'protocol': 1, 'target': args.target, 'license': 'BSD-3-Clause',
               'upstream': manifest['upstream'], 'revision': manifest['revision'],
               'excluded_gpl_sources': manifest['excluded_gpl_sources'],
               'compiler': compiler, 'flags': flags, 'worker_sha256': digest(binary),
               'compile_inputs': [{'path': p.relative_to(ROOT).as_posix(), 'sha256': digest(p)} for p in sources],
               'features': {'independent_bsd_extents': True, 'xattr_mutation': False}}
    (output / 'ext4-worker-profile.json').write_text(json.dumps(profile, indent=2) + '\n')
    notices = output / 'ext4-worker-licenses'
    notices.mkdir(exist_ok=True)
    shutil.copyfile(own / 'LICENSE', notices / 'xetch-ext4-worker-BSD-3-Clause.txt')
    notice = []
    for item in manifest['files']:
        text = (ROOT / 'vendor/lwext4' / item['path']).read_text()
        # Preserve all leading notices, including BSD-2 tree.h's RCS comments.
        end = text.find('#')
        notice.append(item['path'] + '\n' + text[:end] + '\n')
    (notices / 'lwext4-file-notices.txt').write_text('\n'.join(notice))
    print(binary)


if __name__ == '__main__':
    main()
