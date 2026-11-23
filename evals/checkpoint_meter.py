import argparse
import fcntl
import json
import math
import os
import runpy
import sys
import time
import uuid

import checkpoint

MODEL_ENDPOINTS = ('/messages', '/responses', '/responses/compact', '/chat/completions')
POLL_SECONDS = 0.02


def lock(directory):
    handle = open(os.path.join(directory, '.meter-accounting.lock'), 'a')
    fcntl.flock(handle, fcntl.LOCK_EX)
    return handle


def begin(directory, incarnation, metadata=None, request_id=None):
    intents = os.path.join(directory, 'meter-intents')
    with lock(directory):
        if os.path.exists(os.path.join(directory, 'meter-closed.json')):
            raise checkpoint.ResumeError('attempt accounting is closed')
        checkpoint.durable_directory(intents)
        request_id = request_id or uuid.uuid4().hex
        checkpoint.atomic(os.path.join(intents, request_id + '.intent.json'),
            dict(metadata or {}, request_id=request_id, incarnation=incarnation), True)
    return request_id


def attempt_identity_sha256(directory, run):
    state = os.path.join(directory, 'checkpoint.json')
    attempt = checkpoint.read(state)['identity'] if os.path.isfile(state) else {'run': run}
    return checkpoint.digest(attempt)


def not_dispatched(intent, record):
    if not isinstance(intent, dict) or not isinstance(record, dict):
        return False
    proof = record.get('not_dispatched')
    fields = {'run', 'provider', 'path', 'started', 'request', 'request_id',
        'meter_incarnation', 'intent_sha256', 'status', 'ended', 'error', 'not_dispatched'}
    intent_fields = {'schema', 'provider', 'path', 'model', 'started', 'body_sha256',
        'attempt_identity_sha256', 'request_id', 'incarnation'}
    return (type(intent.get('schema')) is int and intent['schema'] == 3
        and set(intent) == intent_fields and set(record) == fields
        and proof == {'schema': 1, 'reason': 'connect_failed_before_request'}
        and type(proof.get('schema')) is int
        and all(isinstance(intent.get(key), str) and intent[key] for key in
            ('provider', 'path', 'request_id', 'incarnation'))
        and all(isinstance(intent.get(key), str) and len(intent[key]) == 64
            and all(char in '0123456789abcdef' for char in intent[key])
            for key in ('body_sha256', 'attempt_identity_sha256'))
        and record.get('intent_sha256') == checkpoint.digest(intent)
        and isinstance(record.get('run'), str) and bool(record['run'])
        and record.get('request_id') == intent['request_id']
        and record.get('meter_incarnation') == intent['incarnation']
        and all(record.get(key) == intent[key] for key in ('provider', 'path', 'started'))
        and isinstance(record.get('request'), dict)
        and record['request'].get('model') == intent.get('model')
        and type(record.get('status')) is int and record['status'] == 0
        and isinstance(record.get('error'), str) and bool(record['error'])
        and type(record.get('ended')) in (int, float)
        and type(intent.get('started')) in (int, float)
        and math.isfinite(record['ended']) and math.isfinite(intent['started'])
        and record['ended'] >= intent['started'])


def bound_record(intent, record, directory=None):
    if 'not_dispatched' in record:
        return (not_dispatched(intent, record) and directory is not None
            and intent['attempt_identity_sha256'] == attempt_identity_sha256(directory, record['run']))
    if intent.get('schema') not in (2, 3):
        return 'schema' not in intent and 'intent_sha256' not in record
    terminal = record.get('terminal') or {}
    return (record.get('intent_sha256') == checkpoint.digest(intent)
        and all(record.get(key) == intent.get(key) for key in ('provider', 'path', 'started'))
        and (record.get('request') or {}).get('model') == intent.get('model')
        and terminal.get('usage_sha256') == checkpoint.digest(record.get('usage'))
        and (not record.get('usage_known') or (terminal.get('complete') is True
            and terminal.get('identity_matches') is True
            and isinstance(terminal.get('response_sha256'), str)
            and isinstance(terminal.get('provider_response_id'), str)
            and terminal.get('model') is not None and terminal.get('model') == intent.get('model'))))


def finish(directory, request_id, record):
    root = os.path.join(directory, 'meter-intents')
    intent = checkpoint.read(os.path.join(root, request_id + '.intent.json'))
    if not bound_record(intent, record, directory):
        raise checkpoint.ResumeError('terminal receipt does not match durable intent')
    receipt = {'request_id': request_id, 'record': record, 'record_sha256': checkpoint.digest(record)}
    if intent.get('schema') in (2, 3) and 'not_dispatched' not in record:
        receipts = os.path.join(directory, 'meter-receipts')
        checkpoint.durable_directory(receipts)
        checkpoint.atomic(os.path.join(receipts, request_id + '.terminal.json'),
            dict(receipt, intent_sha256=checkpoint.digest(intent), terminal=record['terminal']), True)
    checkpoint.atomic(os.path.join(root, request_id + '.done.json'), receipt, True)


def snapshot(directory, incarnation, bound_intents=None):
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
        if not isinstance(record, dict) or receipt.get('request_id') != key or record.get('request_id') != key or record.get('meter_incarnation') != intent['incarnation'] or checkpoint.digest(record) != receipt.get('record_sha256') or not bound_record(intent, record, directory):
            status['unknown'] += 1
            continue
        status['completed'] += 1
        records.append(record)
        if bound_intents is not None:
            bound_intents[key] = intent
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
