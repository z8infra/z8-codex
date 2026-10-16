import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).with_name('check-public-privacy.py')
SCANNER = os.environ.get('GITLEAKS_BIN') or shutil.which('gitleaks')


@unittest.skipUnless(SCANNER, 'Verified Gitleaks executable is required')
class PrivacyEnforcement(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='privacy-test-')
        self.repo = Path(self.temporary.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'z8infra')
        self.git('config', 'user.email', '333475752+z8infra@users.noreply.github.com')
        self.write('.public-privacy.json', json.dumps({'version': 1, 'exceptions': []}))
        self.write('README.md', 'Public documentation\n')
        self.git('add', '--all')

    def tearDown(self):
        self.temporary.cleanup()

    def git(self, *args, env=None):
        return subprocess.run(['git', '-C', str(self.repo), *args], capture_output=True, check=True, env=env).stdout

    def write(self, name, content):
        (self.repo / name).write_text(content, encoding='utf-8')

    def gate(self, mode='staged'):
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.repo), '--gitleaks', SCANNER, '--mode', mode], capture_output=True)
        return result.returncode, json.loads(result.stdout)

    def test_clean_snapshot_passes(self):
        self.assertEqual(self.gate()[0], 0)

    def test_unstaged_file_does_not_replace_index_snapshot(self):
        self.write('README.md', 'Contact private-person@private-domain.invalid')
        self.assertEqual(self.gate()[0], 0)
        self.git('add', 'README.md')
        status, report = self.gate()
        self.assertEqual(status, 1)
        self.assertIn('email', [row['kind'] for row in report['findings']])
        self.assertNotIn('private-person@', json.dumps(report))

    def test_personal_committer_is_rejected_even_with_anonymous_author(self):
        self.git('commit', '-qm', 'public fixture', env=dict(os.environ, GIT_COMMITTER_NAME='Private person', GIT_COMMITTER_EMAIL='private@private-domain.invalid'))
        status, report = self.gate('tree')
        self.assertEqual(status, 1)
        self.assertIn('non_anonymous_committer', [row['kind'] for row in report['findings']])
        self.assertNotIn('non_anonymous_author', [row['kind'] for row in report['findings']])

    def test_history_checks_old_committer_after_new_anonymous_head(self):
        self.git('commit', '-qm', 'first', env=dict(os.environ, GIT_COMMITTER_NAME='Private person', GIT_COMMITTER_EMAIL='private@private-domain.invalid'))
        self.git('commit', '--allow-empty', '-qm', 'anonymous head')
        self.assertEqual(self.gate('tree')[0], 0)
        self.assertEqual(self.gate('history')[0], 1)

    def test_secret_inside_archive_is_rejected(self):
        token = 'ghp_' + hashlib.sha256(b'synthetic unit test fixture').hexdigest()[:36]
        with zipfile.ZipFile(self.repo / 'bundle.zip', 'w') as archive:
            archive.writestr('settings.txt', 'token = "' + token + '"')
        self.git('add', 'bundle.zip')
        status, report = self.gate()
        self.assertEqual(status, 1)
        self.assertTrue(any(row['kind'].startswith('gitleaks:') for row in report['findings']))
        self.assertNotIn(token, json.dumps(report))

    def test_split_encoded_asset_is_reassembled_and_scanned(self):
        token = 'ghp_' + hashlib.sha256(b'synthetic encoded fixture').hexdigest()[:36]
        encoded = base64.b64encode(gzip.compress(('token = "' + token + '"').encode()))
        middle = len(encoded) // 2
        with zipfile.ZipFile(self.repo / 'bundle.zip', 'w') as archive:
            archive.writestr('widget.gz.b64.part001', encoded[:middle])
            archive.writestr('widget.gz.b64.part002', encoded[middle:])
        self.git('add', 'bundle.zip')
        status, report = self.gate()
        self.assertEqual(status, 1)
        self.assertEqual(report['stats']['decoded_assets'], 1)
        self.assertTrue(any(row['kind'].startswith('gitleaks:') for row in report['findings']))

    def test_sensitive_archive_filename_is_rejected(self):
        with zipfile.ZipFile(self.repo / 'bundle.zip', 'w') as archive:
            archive.writestr('.env.production', 'EMPTY=1')
        self.git('add', 'bundle.zip')
        self.assertIn('sensitive_file', [row['kind'] for row in self.gate()[1]['findings']])

    def test_corrupt_archive_fails_closed(self):
        (self.repo / 'broken.zip').write_bytes(b'PK\x03\x04broken')
        self.git('add', 'broken.zip')
        self.assertIn('archive_decode_error', [row['kind'] for row in self.gate()[1]['findings']])

    def test_exact_exception_does_not_allow_another_email(self):
        allowed = 'known-author@private-domain.invalid'
        self.write('.public-privacy.json', json.dumps({'version': 1, 'exceptions': [{'kind': 'email', 'path': 'README.md', 'sha256': hashlib.sha256(allowed.encode()).hexdigest(), 'reason': 'Deliberate public attribution'}]}))
        self.write('README.md', allowed)
        self.git('add', '--all')
        self.assertEqual(self.gate()[0], 0)
        self.write('README.md', allowed + '\nnew-person@private-domain.invalid')
        self.git('add', 'README.md')
        self.assertEqual(self.gate()[0], 1)

    def test_explicit_ref_ignores_unrelated_private_branch(self):
        self.git('commit', '-qm', 'public root')
        self.git('checkout', '-qb', 'private-branch')
        self.git('commit', '--allow-empty', '-qm', 'private work', env=dict(os.environ, GIT_COMMITTER_NAME='Private person', GIT_COMMITTER_EMAIL='private@private-domain.invalid'))
        self.git('checkout', '-')
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.repo), '--gitleaks', SCANNER, '--mode', 'history', '--ref', 'HEAD'], capture_output=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.gate('history')[0], 1)

    def test_private_annotated_tagger_is_rejected(self):
        self.git('commit', '-qm', 'public root')
        self.git('tag', '-a', 'v0.0.1', '-m', 'version', env=dict(os.environ, GIT_COMMITTER_NAME='Private person', GIT_COMMITTER_EMAIL='private@private-domain.invalid'))
        self.assertIn('non_anonymous_tagger', [row['kind'] for row in self.gate('history')[1]['findings']])

    def test_missing_scanner_is_failure(self):
        result = subprocess.run([sys.executable, str(SCRIPT), '--repo', str(self.repo), '--gitleaks', str(self.repo / 'missing.exe')], capture_output=True)
        self.assertEqual(result.returncode, 2)


if __name__ == '__main__':
    unittest.main()
