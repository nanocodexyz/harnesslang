import hashlib
import os
import platform
import sys
import subprocess

from checkpoint import ResumeError, digest

INPUT_FLAGS = ('model', 'agents', 'attempts', 'limits', 'agent_memory_mb', 'max_turns', 'hl_agent_arg', 'openai_route', 'meter_host', 'meter_port', 'relay_port', 'parallel', 'only')


def file_digest(path):
    value = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def tree(directory):
    files = {}
    for base, directories, names in os.walk(directory, followlinks=False):
        directories.sort()
        if any(os.path.islink(os.path.join(base, name)) for name in directories):
            raise ResumeError('symlink directory in input tree: ' + base)
        for name in sorted(names):
            path = os.path.join(base, name)
            relative = os.path.relpath(path, directory)
            files[relative] = file_digest(path)
    if not files:
        raise ResumeError('empty input tree: ' + directory)
    return files


def campaign(args, rows, task_digest, configs, image_ids):
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    source = subprocess.run(['git', '-C', root, 'rev-parse', 'HEAD'], check=True, capture_output=True, text=True).stdout.strip()
    sources = {}
    for directory in ('src', 'hosts', 'agents', 'compiler', 'evals', 'scripts', 'examples'):
        path = os.path.join(root, directory)
        for base, directories, names in os.walk(path):
            directories[:] = sorted(name for name in directories if name not in ('target', '__pycache__', 'node_modules', '.git'))
            for name in sorted(names):
                if name.endswith(('.rs', '.py', '.sh', '.hl', '.toml', '.lock', '.md', '.json')):
                    item = os.path.join(base, name)
                    sources[os.path.relpath(item, root)] = file_digest(item)
    for name in ('Cargo.toml', 'Cargo.lock', 'build.rs', 'AGENTS.md'):
        path = os.path.join(root, name)
        if os.path.isfile(path):
            sources[name] = file_digest(path)
    task_dirs = {row['source'] + '/' + row['task']: row['dir'] for row in rows}
    tasks = {key: task_digest(directory) for key, directory in task_dirs.items()}
    parameters = {key: getattr(args, key) for key in INPUT_FLAGS}
    transport_names = ('HTTPS_PROXY', 'https_proxy', 'HTTP_PROXY', 'http_proxy', 'NO_PROXY', 'no_proxy', 'NANOEVAL_CHATGPT_UPSTREAM', 'NANOEVAL_OPENAI_UPSTREAM', 'NANOEVAL_ANTHROPIC_UPSTREAM', 'NANOEVAL_METER_CLIENTS')
    transport_sha256 = digest({key: os.environ.get(key) for key in transport_names})
    runtime = {'system': platform.system(), 'release': platform.release(), 'machine': platform.machine(), 'python': platform.python_version(), 'implementation': sys.implementation.name}
    return {'source_commit': source, 'source_files': sources, 'parameters': parameters,
            'suite_sha256': file_digest(args.suite), 'agent_tree': tree(os.path.abspath(args.agent_root)),
            'tasks': tasks, 'configs_sha256': digest(configs),
            'images': image_ids, 'matrix': [row['id'] for row in rows],
            'ca_sha256': file_digest(args.ca_bundle) if args.ca_bundle else None,
            'transport_sha256': transport_sha256, 'runtime': runtime,
            'task_trees': {key: digest(tree(directory)) for key, directory in task_dirs.items()},
            'cache': getattr(args, 'cache_identity', {'enabled': False}), 'meter_ledger_schema': 1}
