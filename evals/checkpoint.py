import fcntl
import hashlib
import json
import os
import tempfile
import time

STAGES = ('prepared', 'agent', 'verifier', 'completed')


class ResumeError(RuntimeError):
    pass


class Unresumable(ResumeError):
    pass


def read(path):
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, ValueError) as error:
        raise ResumeError('invalid checkpoint: ' + path) from error


def sync_directory(directory):
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)



def durable_directory(path):
    path = os.path.abspath(path)
    missing = []
    current = path
    for _ in range(len(path.split(os.sep))):
        if os.path.isdir(current):
            break
        missing.append(current)
        current = os.path.dirname(current)
    os.makedirs(path, exist_ok=True)
    for directory in reversed(missing):
        sync_directory(os.path.dirname(directory))
        sync_directory(directory)

def atomic(path, value, exclusive=False):
    directory = os.path.dirname(os.path.abspath(path))
    descriptor, temporary = tempfile.mkstemp(prefix='.checkpoint-', dir=directory)
    try:
        with os.fdopen(descriptor, 'w') as handle:
            json.dump(value, handle, sort_keys=True, indent=1, allow_nan=False)
            handle.write('\n')
            handle.flush()
            os.fsync(handle.fileno())
        if exclusive:
            os.link(temporary, path)
            os.unlink(temporary)
        else:
            os.replace(temporary, path)
        sync_directory(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()).hexdigest()


class Campaign:
    def __init__(self, directory, identity, resume):
        durable_directory(directory)
        self.lock = open(os.path.join(directory, '.campaign.lock'), 'a')
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ResumeError('campaign already has an owner') from error
        path = os.path.join(directory, 'campaign.json')
        if os.path.exists(path):
            if not resume:
                raise ResumeError('campaign exists; use --resume')
            if read(path)['identity'] != identity:
                raise ResumeError('campaign identity mismatch; no work dispatched')
        else:
            if resume:
                raise ResumeError('--resume requires campaign.json; legacy evidence is read-only')
            rows = os.path.join(directory, 'rows')
            if os.path.isdir(rows) and os.listdir(rows):
                raise ResumeError('refusing untracked row evidence')
            atomic(path, {'schema': 1, 'identity': identity, 'created': time.time()}, True)
        self.identity = digest(identity)

    def close(self):
        self.lock.close()


class Attempt:
    def __init__(self, directory, identity):
        self.directory = directory
        self.path = os.path.join(directory, 'checkpoint.json')
        durable_directory(directory)
        if os.path.exists(self.path):
            self.value = read(self.path)
            if self.value['identity'] != identity or self.value['stage'] not in STAGES:
                raise ResumeError('attempt identity or stage mismatch')
        else:
            if os.listdir(directory):
                raise ResumeError('attempt has evidence without checkpoint')
            self.value = {'schema': 1, 'identity': identity, 'stage': 'prepared'}
            atomic(self.path, self.value, True)

    def save(self):
        atomic(self.path, self.value)

    def phase(self, stage, timeout):
        if stage not in ('agent', 'verifier'):
            raise ResumeError('invalid phase')
        if self.value['stage'] == stage:
            return False
        expected = 'prepared' if stage == 'agent' else 'agent'
        if self.value['stage'] != expected:
            raise ResumeError('invalid phase transition')
        self.value.update(stage=stage, deadline=time.time() + timeout, started=time.time())
        self.save()
        return True

    def complete(self, result):
        path = os.path.join(self.directory, 'result.json')
        if os.path.exists(path):
            if read(path) != result:
                raise ResumeError('completed row is immutable')
        else:
            atomic(path, result, True)
        self.value.update(stage='completed', result_sha256=digest(result))
        self.save()
        return result

    def completed(self):
        path = os.path.join(self.directory, 'result.json')
        if not os.path.exists(path):
            if self.value['stage'] == 'completed':
                raise ResumeError('completed row lost result')
            return None
        result = read(path)
        if self.value['stage'] == 'completed':
            if self.value['result_sha256'] != digest(result):
                raise ResumeError('completed result changed')
            return result
        return self.complete(result)
