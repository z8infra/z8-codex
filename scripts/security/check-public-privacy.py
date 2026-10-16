#!/usr/bin/env python3
"""Fail-closed privacy checks for public Git snapshots. Output never contains values."""
import argparse
import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import zipfile

NAME = 'z8infra'
EMAIL = '333475752+z8infra@users.noreply.github.com'
EMAIL_RE = re.compile(r'(?<![\w.+-])[\w.+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?![\w.-])')
PATH_RE = re.compile(r'''(?:[A-Za-z]:[\\/]+Users[\\/]+[^\\/\s"'<>]+|/(?:Users|home)/[^/\s"'<>]+)''')
SENSITIVE = re.compile(r'(?i)(?:^|/)(?:\.env(?:\.[^/]*)?|auth\.json|credentials(?:\.[^/]*)?|id_(?:rsa|ed25519)|cookies?(?:\.[^/]*)?|[^/]*\.(?:p12|pfx|pem|key|db|sqlite|sqlite3|log|har|dmp))$')
EXAMPLES = re.compile(r'(?i)(?:\.example|\.sample|\.template)$')
PUBLIC_EMAILS = {EMAIL, 'support@z8.hk', 'user@im.wechat', 'alice@im.wechat', 'bob@im.wechat'}
MAX_BYTES = 100 * 1024 * 1024


def digest(value):
    return hashlib.sha256(value.encode('utf-8') if isinstance(value, str) else value).hexdigest()


def git(repo, *args):
    result = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError('git operation failed: ' + args[0])
    return result.stdout


class Gate:
    def __init__(self, repo, folder, policy):
        self.repo, self.folder, self.policy = repo, folder, policy
        self.findings, self.corpus, self.seen = [], {}, set()
        self.stats = {'blobs': 0, 'texts': 0, 'archives': 0, 'decoded_assets': 0, 'binary_items': 0, 'allowed': 0}

    def issue(self, kind, path, line=None, value=None):
        row = {'kind': kind, 'path': path}
        if line is not None:
            row['line'] = line
        if value is not None:
            row['fingerprint'] = digest(value)
        self.findings.append(row)

    def allowed(self, kind, path, value, rule=None):
        for row in self.policy.get('exceptions', []):
            if row.get('kind') == kind and row.get('path') == path and row.get('sha256') == digest(value) and row.get('reason') and row.get('rule') == rule:
                self.stats['allowed'] += 1
                return True
        return False

    def identity(self, ref=None):
        if ref is None:
            values = [git(self.repo, 'var', 'GIT_AUTHOR_IDENT').decode().split('>')[0] + '>', git(self.repo, 'var', 'GIT_COMMITTER_IDENT').decode().split('>')[0] + '>']
            expected = f'{NAME} <{EMAIL}>'
            for role, value in zip(('author', 'committer'), values):
                if value != expected:
                    self.issue('non_anonymous_' + role, '[effective Git identity]')
        else:
            values = git(self.repo, 'show', '-s', '--format=%an%x00%ae%x00%cn%x00%ce', ref).decode().rstrip('\n').split('\0')
            for role, name, email in [('author', *values[:2]), ('committer', *values[2:])]:
                if (name, email) != (NAME, EMAIL):
                    self.issue('non_anonymous_' + role, ref)

    def content(self, data, source, depth=0):
        if len(data) > MAX_BYTES or depth > 5:
            self.issue('scan_limit_exceeded', source)
            return
        if SENSITIVE.search(source.split('!')[-1]) and not EXAMPLES.search(source):
            if not self.allowed('sensitive_file', source, data):
                self.issue('sensitive_file', source)
        if data.startswith(b'PK\x03\x04'):
            self.stats['archives'] += 1
            try:
                with zipfile.ZipFile(io.BytesIO(data)) as archive:
                    infos = archive.infolist()
                    if sum(item.file_size for item in infos) > MAX_BYTES:
                        self.issue('archive_size_limit', source)
                        return
                    parts = {}
                    for item in infos:
                        if item.is_dir():
                            continue
                        path = item.filename.replace('\\', '/')
                        body = archive.read(item)
                        match = re.fullmatch(r'(.*\.gz\.b64)\.part(\d+)', path)
                        if match:
                            parts.setdefault(match[1], []).append((int(match[2]), body))
                        else:
                            self.content(body, source + '!' + path, depth + 1)
                    for path, pieces in parts.items():
                        pieces.sort()
                        if [n for n, _ in pieces] != list(range(1, len(pieces) + 1)):
                            self.issue('missing_archive_part', source + '!' + path)
                            continue
                        with gzip.GzipFile(fileobj=io.BytesIO(base64.b64decode(re.sub(rb'\s+', b'', b''.join(body for _, body in pieces)), validate=True))) as stream:
                            decoded = stream.read(MAX_BYTES + 1)
                        self.stats['decoded_assets'] += 1
                        self.content(decoded, source + '!' + path + '[decoded]', depth + 1)
            except (OSError, ValueError, EOFError, zipfile.BadZipFile, RuntimeError):
                self.issue('archive_decode_error', source)
            return
        if data.startswith(b'\x1f\x8b'):
            try:
                with gzip.GzipFile(fileobj=io.BytesIO(data)) as stream:
                    self.content(stream.read(MAX_BYTES + 1), source + '![gzip]', depth + 1)
            except (OSError, ValueError, EOFError):
                self.issue('gzip_decode_error', source)
            return
        if b'\0' in data[:8192]:
            self.stats['binary_items'] += 1
            strings = re.findall(rb'[\x20-\x7e]{6,}', data)
            strings.extend(item.decode('utf-16-le').encode() for item in re.findall(rb'(?:[\x20-\x7e]\x00){6,}', data))
            if strings:
                self.content(b'\n'.join(strings), source + '![binary strings]', depth + 1)
            return
        try:
            text = data.decode('utf-8-sig')
        except UnicodeDecodeError:
            self.issue('unsupported_text_encoding', source)
            return
        key = digest(data)
        self.corpus.setdefault(key, set()).add(source)
        if key not in self.seen:
            (self.folder / (key + '.txt')).write_bytes(data)
            self.seen.add(key)
            self.stats['texts'] += 1
        for kind, pattern in [('email', EMAIL_RE), ('local_user_path', PATH_RE)]:
            for match in pattern.finditer(text):
                value = match.group()
                if kind == 'email' and (value in PUBLIC_EMAILS or value.endswith('@users.noreply.github.com') or re.search(r'@(?:example\.(?:com|org|net|test)|yourdomain\.com)$', value) or re.fullmatch(r'icon_\d+x\d+@2x\.png', value)):
                    continue
                if not self.allowed(kind, source, value):
                    self.issue(kind, source, text.count('\n', 0, match.start()) + 1, value)

    def scan_refs(self, mode, ref):
        commits = []
        if mode == 'staged':
            self.identity()
            entries = git(self.repo, 'ls-files', '--stage', '-z').split(b'\0')
        else:
            commits = git(self.repo, 'rev-list', *([ref] if ref else ['--branches', '--tags', 'HEAD'])).decode().splitlines() if mode == 'history' else [git(self.repo, 'rev-parse', (ref or 'HEAD') + '^{commit}').decode().strip()]
            entries = []
            for commit in commits:
                self.identity(commit)
                entries.extend(git(self.repo, 'ls-tree', '-rz', '--full-tree', commit).split(b'\0'))
            if mode == 'history':
                for tag in ([ref] if ref else git(self.repo, 'for-each-ref', '--format=%(refname)', 'refs/tags').decode().splitlines()):
                    if git(self.repo, 'cat-file', '-t', tag).strip() == b'tag':
                        body = git(self.repo, 'cat-file', 'tag', tag).decode('utf-8', 'replace')
                        tagger = re.search(r'^tagger (.*?) <(.*?)>', body, re.M)
                        if not tagger or tagger.groups() != (NAME, EMAIL):
                            self.issue('non_anonymous_tagger', tag)
                        self.content(body.split('\n\n', 1)[-1].encode(), '[tag-message]:' + tag)
        processed = set()
        for entry in entries:
            if not entry:
                continue
            header, raw_path = entry.split(b'\t', 1)
            mode_bits, kind, tail = header.decode().split()
            oid = kind if mode == 'staged' else tail
            path = raw_path.decode('utf-8', 'replace')
            if mode_bits == '160000':
                self.issue('unscanned_submodule', path)
                continue
            if mode == 'staged' and tail != '0':
                self.issue('unmerged_index', path)
                continue
            if (oid, path) in processed:
                continue
            processed.add((oid, path))
            self.stats['blobs'] += 1
            self.content(git(self.repo, 'cat-file', 'blob', oid), path)
        for commit in commits:
            self.content(git(self.repo, 'show', '-s', '--format=%B', commit), '[commit-message]:' + commit)

    def secrets(self, executable):
        config = self.folder.parent / 'scanner.toml'
        config.write_text('[extend]\nuseDefault = true\n', encoding='utf-8')
        output = self.folder.parent / 'scanner.json'
        environment = dict(os.environ)
        environment.pop('GITLEAKS_CONFIG', None)
        environment.pop('GITLEAKS_CONFIG_TOML', None)
        result = subprocess.run([executable, 'dir', str(self.folder), '--config', str(config), '--gitleaks-ignore-path', str(self.folder), '--ignore-gitleaks-allow', '--max-decode-depth=5', '--report-format=json', '--report-path', str(output), '--no-banner', '--no-color', '--exit-code=0'], capture_output=True, env=environment)
        if result.returncode or not output.exists() or re.search(rb'\b(?:ERR|FTL)\b', result.stderr):
            raise RuntimeError('Gitleaks did not complete; output withheld')
        for row in json.loads(output.read_text(encoding='utf-8')):
            key = Path(row['File']).stem
            if key not in self.corpus:
                raise RuntimeError('Unmapped scanner result')
            for source in sorted(self.corpus[key]):
                if not self.allowed('gitleaks', source, row['Secret'], row['RuleID']):
                    self.issue('gitleaks:' + row['RuleID'], source, row['StartLine'], row['Secret'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    parser.add_argument('--mode', choices=['staged', 'tree', 'history'], default='staged')
    parser.add_argument('--ref', help='Limit tree/history to this ref; history defaults to HEAD plus all branches and tags')
    parser.add_argument('--gitleaks', default=os.environ.get('GITLEAKS_BIN') or shutil.which('gitleaks'))
    args = parser.parse_args()
    try:
        if not args.gitleaks:
            raise RuntimeError('Gitleaks is required; install the verified scanner first')
        policy_bytes = git(args.repo, 'show', ':.public-privacy.json' if args.mode == 'staged' else (args.ref or 'HEAD') + ':.public-privacy.json')
        policy = json.loads(policy_bytes)
        if policy.get('version') != 1:
            raise RuntimeError('Unsupported privacy exception schema')
        with tempfile.TemporaryDirectory(prefix='z8-privacy-') as temporary:
            corpus = Path(temporary) / 'corpus'
            corpus.mkdir()
            gate = Gate(args.repo, corpus, policy)
            gate.scan_refs(args.mode, args.ref)
            gate.secrets(args.gitleaks)
            print(json.dumps({'ok': not gate.findings, 'mode': args.mode, 'stats': gate.stats, 'findings': gate.findings}, ensure_ascii=True, indent=2))
            return int(bool(gate.findings))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        print(json.dumps({'ok': False, 'error': 'Privacy check could not complete. Verify Git, the staged/selected policy, archive integrity and Gitleaks; no raw output is emitted.'}))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
