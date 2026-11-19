import argparse
import fcntl
import json
import os
import runpy
import sys
import time
import uuid

import checkpoint

MODEL_ENDPOINTS = ('/messages', '/responses', '/responses/compact')
POLL_SECONDS = 0.02


def lock(directory):
    handle = open(os.path.join(directory, '.meter-accounting.lock'), 'a')
    fcntl.flock(handle, fcntl.LOCK_EX)
    return handle


def begin(directory, incarnation):
    intents = os.path.join(directory, 'meter-intents')
    with lock(directory):
        if os.path.exists(os.path.join(directory, 'meter-closed.json')):
            raise checkpoint.ResumeError('attempt accounting is closed')
        checkpoint.durable_directory(intents)
        request_id = uuid.uuid4().hex
        checkpoint.atomic(os.path.join(intents, request_id + '.intent.json'),
            {'request_id': request_id, 'incarnation': incarnation}, True)
    return request_id


def finish(directory, request_id, record):
    checkpoint.atomic(os.path.join(directory, 'meter-intents', request_id + '.done.json'),
        {'request_id': request_id, 'record': record, 'record_sha256': checkpoint.digest(record)}, True)


def snapshot(directory, incarnation):
    root = os.path.join(directory, 'meter-intents')
    intents = sorted(name for name in os.listdir(root) if name.endswith('.intent.json')) if os.path.isdir(root) else []
    status = {'intents': len(intents), 'completed': 0, 'active': 0, 'unknown': 0, 'unmatched_records': 0}
    records, keys, completed_hashes = [], set(), {}
    for name in intents:
        intent = checkpoint.read(os.path.join(root, name))
        key = intent['request_id']
        if name != key + '.intent.json' or key in keys:
            raise checkpoint.ResumeError('invalid durable request identifier')
        keys.add(key)
        done = os.path.join(root, key + '.done.json')
        if not os.path.exists(done):
            status['active' if intent['incarnation'] == incarnation else 'unknown'] += 1
            continue
        receipt = checkpoint.read(done)
        record = receipt.get('record')
        if not isinstance(record, dict) or receipt.get('request_id') != key or record.get('request_id') != key or record.get('meter_incarnation') != intent['incarnation'] or checkpoint.digest(record) != receipt.get('record_sha256'):
            status['unknown'] += 1
            continue
        status['completed'] += 1
        records.append(record)
        completed_hashes[key] = receipt['record_sha256']
    path = os.path.join(directory, 'requests.jsonl')
    if os.path.isfile(path):
        seen = set()
        with open(path) as handle:
            for line in handle:
                try:
                    record = json.loads(line)
                except ValueError:
                    status['unmatched_records'] += 1
                    continue
                key = record.get('request_id')
                if key not in keys:
                    status['unmatched_records'] += 1
                    records.append(record)
                elif key in seen or completed_hashes.get(key) != checkpoint.digest(record):
                    status['unmatched_records'] += 1
                seen.add(key)
    return status, records


def reconcile(directory, incarnation, timeout=None):
    if timeout is None:
        import meter
        timeout = meter.UPSTREAM_TIMEOUT_SEC
    with lock(directory):
        closed = os.path.join(directory, 'meter-closed.json')
        if not os.path.exists(closed):
            checkpoint.atomic(closed, {'incarnation': incarnation}, True)
    deadline = time.monotonic() + timeout
    polls_max = int(timeout / POLL_SECONDS) + 2
    for _ in range(polls_max):
        status, _ = snapshot(directory, incarnation)
        if not status['active'] or time.monotonic() >= deadline:
            status['unknown'] += status.pop('active')
            status['closed'] = True
            return status
        time.sleep(POLL_SECONDS)
    raise checkpoint.ResumeError('meter drain bound exhausted')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--role', choices=('meter', 'relay'), required=True)
    parser.add_argument('--owner', required=True)
    parser.add_argument('--environment-sha256', required=True)
    parser.add_argument('--state', required=True)
    parser.add_argument('arguments', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    incarnation = uuid.uuid4().hex
    checkpoint.atomic(args.state, {'owner': args.owner, 'role': args.role, 'incarnation': incarnation,
        'environment_sha256': args.environment_sha256, 'pid': os.getpid()})
    if args.role == 'relay':
        sys.argv = ['relay.py'] + args.arguments
        runpy.run_path(os.path.join(os.path.dirname(__file__), 'relay.py'), run_name='__main__')
        return
    import meter
    meter.Meter.incarnation = incarnation
    sys.argv = ['meter.py'] + args.arguments
    meter.main()


if __name__ == '__main__':
    main()
