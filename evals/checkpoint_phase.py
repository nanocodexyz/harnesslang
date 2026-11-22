import json
import os
import time

from checkpoint import ResumeError, Unresumable, digest, read

POLL_SECONDS = 0.2
JOURNAL_MOUNT = '/opt/nanoeval-journal'


def container(sh, name):
    result = sh(['docker', 'inspect', name], check=False)
    if result.returncode:
        raise Unresumable('preserved container missing; no fresh attempt permitted')
    return json.loads(result.stdout)[0]


def validate_container(sh, name, state, journal, agent_root):
    info = container(sh, name)
    expected = {JOURNAL_MOUNT: os.path.abspath(journal), '/opt/nanoeval-agent': agent_root}
    mounts = {mount['Destination']: mount['Source'] for mount in info['Mounts']}
    if info['Image'] != state['image_id'] or any(mounts.get(key) != value for key, value in expected.items()):
        raise ResumeError('preserved container image or mounts changed')
    return info


def phase(sh, name, row, env, argv, timeout, journal, stage, checkpoint):
    fresh = checkpoint.phase(stage, timeout)
    token = digest([checkpoint.value['identity'], stage])
    receipt = os.path.join(journal, stage + '.receipt.json')
    if fresh:
        launch(sh, name, row, env, argv, stage, token)
    else:
        if checkpoint.value.get('accounting_status') in ('observed', 'unknown_at_timeout'):
            checkpoint.value['accounting_status'] = 'unknown_after_interruption'
        checkpoint.save()
    return await_receipt(sh, name, journal, stage, token, receipt, checkpoint, not fresh)


def launch(sh, name, row, env, argv, stage, token):
    prefix = JOURNAL_MOUNT + '/' + stage
    script = 'echo $$ > "$1.pid"; prefix=$1; token=$2; shift 2; '
    script += '"$@" >> "$prefix.stdout" 2>> "$prefix.stderr"; code=$?; '
    script += 'printf \'{"exit":%s,"token":"%s"}\\n\' "$code" "$token" > "$prefix.receipt.tmp"; '
    script += 'sync; mv "$prefix.receipt.tmp" "$prefix.receipt.json"; sync'
    command = ['docker', 'exec', '-d', '-w', row['workdir']]
    for key, value in env.items():
        command += ['-e', key + '=' + value]
    sh(command + [name, 'sh', '-c', script, 'sh', prefix, token] + argv)


def await_receipt(sh, name, journal, stage, token, receipt, checkpoint, interrupted):
    remaining = checkpoint.value['deadline'] - time.time()
    polls_max = max(1, int(max(0, remaining) / POLL_SECONDS) + 2)
    for _ in range(polls_max):
        if os.path.exists(receipt):
            if os.stat(receipt).st_mtime > checkpoint.value["deadline"]:
                raise Unresumable(stage + " receipt after original deadline; no score or replay")
            result = read(receipt)
            if result.get('token') != token or type(result.get('exit')) is not int:
                raise ResumeError('invalid phase receipt')
            return {'exit': result['exit'], 'timed_out': False, 'wall_ms': int((time.time() - checkpoint.value['started']) * 1000)}
        info = container(sh, name)
        if not info['State']['Running']:
            raise Unresumable(stage + ' interrupted: container stopped; journal retained, execution not replayed')
        if time.time() >= checkpoint.value['deadline']:
            sh(['docker', 'stop', '--time', '1', name])
            if interrupted and remaining <= 0:
                raise Unresumable(stage + ' budget expired during interruption; no score or replay')
            if checkpoint.value.get('accounting_status') == 'observed':
                checkpoint.value['accounting_status'] = 'unknown_at_timeout'
            checkpoint.save()
            return {'exit': None, 'timed_out': True, 'wall_ms': int((time.time() - checkpoint.value['started']) * 1000)}
        time.sleep(min(POLL_SECONDS, max(0, checkpoint.value['deadline'] - time.time())))
    raise ResumeError(stage + ' original timeout budget exhausted; execution not replayed')


class ExistingService:
    def __init__(self, pid, command):
        self.pid, self.command = pid, command

    def poll(self):
        try:
            with open('/proc/' + str(self.pid) + '/cmdline', 'rb') as handle:
                actual = handle.read().split(b'\0')[:-1]
            return None if actual == [part.encode() for part in self.command] else 1
        except OSError:
            return 1


def service(command, log, environment, endpoint=None):
    import subprocess
    existing = []
    for pid in os.listdir('/proc'):
        if pid.isdigit():
            process = ExistingService(int(pid), command)
            if process.poll() is None:
                existing.append(process)
    if len(existing) > 1:
        raise ResumeError('multiple campaign services match; refusing ambiguous ownership')
    if existing:
        return existing[0]
    if endpoint:
        import socket
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(endpoint)
            except OSError as error:
                raise ResumeError("foreign or unowned service occupies " + str(endpoint)) from error
    return subprocess.Popen(command, stdout=log, stderr=log, env=environment, start_new_session=True)


def stop_services(processes):
    import signal
    for process in processes:
        command = getattr(process, 'command', getattr(process, 'args', None))
        if command and ExistingService(process.pid, command).poll() is None:
            if os.getpgid(process.pid) != process.pid:
                raise ResumeError('campaign service is not its own process group')
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                continue
            if hasattr(process, "wait"):
                process.wait(timeout=5)
