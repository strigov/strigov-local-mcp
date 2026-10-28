#!/usr/bin/env python3
"""Install the Windows ZIP into isolated folders and exercise the real executables.

Nothing is installed into the user's profile: every path lives under a new
attempt directory, the app always gets an explicit --config, no tray or
scheduled task is started, and no real account or key is used.
"""
import argparse, datetime, hashlib, json, os, pathlib, platform, secrets, shutil, subprocess, sys, zipfile

project = pathlib.Path(__file__).resolve().parent.parent
checks = []


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def tree(root):
    root = pathlib.Path(root)
    return [{'path': p.relative_to(root).as_posix(), 'bytes': p.stat().st_size, 'sha256': sha256(p)}
            for p in sorted(root.rglob('*')) if p.is_file()]


class Run:
    def __init__(self, logs):
        self.logs = logs
        self.count = 0

    def __call__(self, check_id, args, expect_ok=True, env=None, timeout=120, cwd=None):
        self.count += 1
        log = self.logs / f'{self.count:02d}-{check_id}.log'
        try:
            result = subprocess.run([str(a) for a in args], capture_output=True, timeout=timeout,
                                    env=env, cwd=cwd, stdin=subprocess.DEVNULL)
            code, out, err = result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired as error:
            code, out, err = None, error.stdout or b'', (error.stderr or b'') + b'\nTIMEOUT'
        log.write_bytes(b'$ ' + ' '.join(map(str, args)).encode() + f'\nexit={code}\n--- stdout\n'.encode()
                        + out + b'\n--- stderr\n' + err)
        ok = code == 0 if expect_ok else (code is not None and code != 0)
        return code, out.decode('utf-8', 'replace'), ok, log


def record(check_id, ok, code=None, logs=(), detail=None):
    checks.append({'id': check_id, 'status': 'PASS' if ok else 'FAIL', 'exit_code': code,
                   'log_paths': [str(p) for p in logs], **({'detail': detail} if detail else {})})
    print(('PASS ' if ok else 'FAIL ') + check_id + (f' — {detail}' if detail and not ok else ''), flush=True)
    return ok


def tool(args):
    try:
        return subprocess.run(args, capture_output=True, text=True, timeout=30).stdout.strip().splitlines()[0]
    except Exception as error:
        return f'unavailable: {error}'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('zip')
    parser.add_argument('--output-root', required=True)
    args = parser.parse_args()
    if sys.platform != 'win32' or platform.machine().lower() not in ('amd64', 'x86_64'):
        raise SystemExit('Native Windows x64 is required')
    archive = pathlib.Path(args.zip).resolve()
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    attempt = pathlib.Path(args.output_root).resolve() / f'{stamp}-{secrets.token_hex(4)}'
    attempt.mkdir(parents=True)  # never reuse or overwrite earlier evidence
    logs = attempt / 'logs'
    logs.mkdir()
    run = Run(logs)
    digest = sha256(archive)
    sidecar = archive.with_name(archive.name + '.sha256')
    expected = sidecar.read_text(encoding='ascii').split() if sidecar.exists() else []
    record('sidecar-sha256', expected == [digest, archive.name], detail=f'{digest} vs {expected}')

    # Safe extraction: reject absolute, traversal, drive and stream names.
    work = attempt / "QA Program's Files Тест"
    extracted = work / 'extracted'
    with zipfile.ZipFile(archive) as bundle:
        names = bundle.namelist()
        unsafe = [n for n in names if n.startswith(('/', '\\')) or ':' in n or '..' in pathlib.PurePosixPath(n).parts]
        record('zip-members-safe', not unsafe, detail=str(unsafe))
        if unsafe:
            return finish(attempt, archive, digest, [])
        bundle.extractall(extracted)
    layout = {'local-files-installer.exe', 'Read Me.txt', 'payload/local-files.exe',
              'payload/tunnel-client.exe', 'payload/install-manifest.json'}
    record('zip-layout', layout <= set(names), detail=str(sorted(layout - set(names))))
    installer = extracted / 'local-files-installer.exe'

    code, out, ok, log = run('installer-check', [installer, '--check'])
    record('installer-check', ok and 'verified' in out, code, [log])

    target = work / 'Programs' / 'Strigov Local MCP'
    code, _, ok, log = run('install-clean', [installer, '--install-to', target, '--no-launch'])
    record('install-clean', ok and (target / 'local-files.exe').is_file()
           and (target / 'tunnel-client.exe').is_file(), code, [log])
    installed = tree(target) if target.exists() else []
    payload = {e['path']: e for e in tree(extracted / 'payload')}
    record('installed-matches-payload', {e['path']: e for e in installed} == payload)
    exe = target / 'local-files.exe'

    code, out, ok, log = run('app-version', [exe, '--version'])
    record('app-version', ok and out.strip() == 'Strigov Local MCP 2.2.0', code, [log])
    settings = work / 'Настройки пользователя'
    config = settings / 'folders.json'
    code, _, ok, log = run('app-init', [exe, 'init', '--config', config])
    roots = json.loads(config.read_text(encoding='utf-8')).get('roots') if config.exists() else None
    record('app-init-shares-nothing', ok and roots == [], code, [log])
    code, out, ok, log = run('app-check', [exe, 'check', '--config', config])
    record('app-check', ok and '0 folders' in out, code, [log])
    code, out, ok, log = run('app-status', [exe, 'status', '--config', config])
    try:
        status = json.loads(out)
    except ValueError:
        status = {}
    record('app-status-not-connected', ok and status.get('connected') is False, code, [log])

    # Update: settings outside the app folder stay byte-identical, old app goes to backup.
    connection = settings / 'connection.json'
    connection.write_text(json.dumps({'version': 1, 'tunnel_id': 'tunnel_0123456789abcdef0123456789abcdef',
                                      'credential_id': 'connection-qa-synthetic'}), encoding='utf-8')
    before = {p.name: sha256(p) for p in (config, connection)}
    marker = target / 'local-files.exe'
    old_digest = sha256(marker)
    code, _, ok, log = run('reinstall', [installer, '--install-to', target, '--no-launch'])
    backups = [p for p in target.parent.iterdir() if p.name.startswith('Strigov Local MCP backup ')]
    backup_ok = len(backups) == 1 and sha256(backups[0] / target.name / 'local-files.exe') == old_digest
    after = {p.name: sha256(p) for p in (config, connection)}
    record('reinstall-keeps-settings-and-backup', ok and backup_ok and before == after
           and {e['path']: e for e in tree(target)} == payload, code, [log])

    # Busy application: a running installed binary must block replacement.
    worker = subprocess.Popen([str(exe), 'serve', '--config', str(config)], stdin=subprocess.PIPE,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        snapshot = tree(target)
        code, out, ok, log = run('reject-busy', [installer, '--install-to', target, '--no-launch'],
                                 expect_ok=False)
        record('reject-busy-unchanged', ok and tree(target) == snapshot and worker.poll() is None
               and len([p for p in target.parent.iterdir() if p.name.startswith('Strigov Local MCP backup ')]) == 1,
               code, [log])
    finally:
        worker.stdin.close()
        try:
            worker.wait(10)
        except subprocess.TimeoutExpired:
            worker.kill()

    foreign = work / 'Foreign'
    foreign.mkdir()
    (foreign / 'notes.txt').write_text('keep', encoding='utf-8')
    code, _, ok, log = run('reject-foreign', [installer, '--install-to', foreign, '--no-launch'], expect_ok=False)
    record('reject-foreign-unchanged', ok and [p.name for p in foreign.iterdir()] == ['notes.txt'], code, [log])

    real = work / 'junction-target'
    real.mkdir()
    link = work / 'junction'
    made = subprocess.run(['cmd', '/c', 'mklink', '/J', str(link), str(real)], capture_output=True)
    if made.returncode == 0:
        code, _, ok, log = run('reject-reparse', [installer, '--install-to', link / 'App', '--no-launch'],
                               expect_ok=False)
        record('reject-reparse-ancestor', ok and not any(real.iterdir()), code, [log])
        os.rmdir(link)
    else:
        record('reject-reparse-ancestor', False, detail='could not create junction fixture')

    corrupt = work / 'corrupt'
    shutil.copytree(extracted, corrupt)
    runtime = corrupt / 'payload' / 'tunnel-client.exe'
    data = bytearray(runtime.read_bytes())
    data[-1] ^= 0xFF
    runtime.write_bytes(bytes(data))
    code, _, ok, log = run('reject-corrupt-check', [corrupt / 'local-files-installer.exe', '--check'], expect_ok=False)
    record('reject-corrupt-check', ok, code, [log])
    fresh = work / 'Corrupt Install'
    code, _, ok, log = run('reject-corrupt-install',
                           [corrupt / 'local-files-installer.exe', '--install-to', fresh, '--no-launch'], expect_ok=False)
    record('reject-corrupt-install', ok and not fresh.exists(), code, [log])
    (corrupt / 'payload' / 'extra.dll').write_bytes(b'x')
    runtime.write_bytes((extracted / 'payload' / 'tunnel-client.exe').read_bytes())
    code, _, ok, log = run('reject-extra-file', [corrupt / 'local-files-installer.exe', '--check'], expect_ok=False)
    record('reject-extra-file', ok, code, [log])

    env = dict(os.environ, NATIVE_BINARY=str(exe), NATIVE_WORKER='1')
    code, out, ok, log = run('installed-worker-mcp', ['node', '--test', 'test/native.test.mjs'],
                             env=env, timeout=300, cwd=project)
    record('installed-worker-mcp', ok, code, [log],
           detail='symlink privilege missing (enable Developer Mode)' if 'EPERM' in log.read_text('utf-8', 'replace') else None)

    env = {k: v for k, v in os.environ.items() if k not in ('NATIVE_BINARY', 'NATIVE_WORKER')}
    env['LOCAL_FILES_TEST_APP'] = str(target)
    code, out, ok, log = run('packaged-runtime', ['cargo', 'test', '--locked', '--bin', 'local-files',
                             'packaged_runtime_accepts_generated_profile_without_external_account', '--', '--ignored'],
                             env=env, timeout=600, cwd=project)
    record('packaged-runtime-local-health', ok and '1 passed' in out, code, [log])
    return finish(attempt, archive, digest, tree(target) if target.exists() else [])


def finish(attempt, archive, digest, installed):
    git = lambda *a: subprocess.run(['git', *a], cwd=project, capture_output=True, text=True).stdout.strip()
    report = {
        'schema_version': 1,
        'artifact': {'path': str(archive), 'bytes': archive.stat().st_size, 'sha256': digest},
        'source': {'git_head': git('rev-parse', 'HEAD'), 'dirty': bool(git('status', '--porcelain'))},
        'platform': {'os': platform.platform(), 'arch': platform.machine()},
        'tools': {'python': platform.python_version(), 'node': tool(['node', '--version']),
                  'cargo': tool(['cargo', '--version']), 'rustc': tool(['rustc', '--version'])},
        'checks': checks,
        'installed_files': installed,
        'user_settings_changed': False,
        'gui_interaction': 'NOT_RUN', 'external_account': 'NOT_RUN', 'system_autostart': 'NOT_RUN',
        'signed': False,
    }
    (attempt / 'verification.json').write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding='utf-8')
    failed = [c['id'] for c in checks if c['status'] != 'PASS']
    print(json.dumps({'verification': str(attempt / 'verification.json'), 'sha256': digest, 'failed': failed},
                     ensure_ascii=False))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
