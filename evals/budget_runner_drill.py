import contextlib
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

import run

REPETITIONS = 3


def exercise(root, case):
    calls = []
    states = [{'halt': None}, {'halt': None}, {'halt': 'cap reserved'}]
    if case == 'route-halt':
        states = [{'halt': None}] * 3
        (root / run.CHATGPT_HALT_FILE).write_text('{}')
    free = [12 * 1024 ** 3, 10 * 1024 ** 3] if case == 'disk' else [12 * 1024 ** 3] * 3
    args = SimpleNamespace(out=str(root), budget_policy={'ledger_path': str(root / 'budget.sqlite3')}, meter_incarnation='fixture',
                           min_free_gib=11, agent_root=str(root))
    rows = [{'id': str(i)} for i in range(3)]
    gate = SimpleNamespace(status=lambda: states.pop(0))

    def result(row, *unused):
        calls.append(row['id'])
        return {'id': row['id'], 'outcome': 'passed'}

    with patch.object(run.budget, 'Gate', return_value=gate), \
            patch.object(run, 'run_row', side_effect=result), \
            patch.object(run.shutil, 'disk_usage', side_effect=lambda path: SimpleNamespace(free=free.pop(0))), \
            contextlib.redirect_stdout(io.StringIO()) as output:
        run.budgeted_rows(args, rows, 'unused', {}, None, str(root))
    expected = {'cap': ['0', '1'], 'disk': ['0'], 'route-halt': []}[case]
    assert calls == expected, (case, calls)
    assert 'stopped' in output.getvalue() or 'halted' in output.getvalue()
    assert not list(root.glob('*/checkpoint.json'))


def main():
    with tempfile.TemporaryDirectory(prefix='budget-runner-') as temporary:
        root = Path(temporary)
        for repetition in range(REPETITIONS):
            for case in ('cap', 'disk', 'route-halt'):
                directory = root / (case + str(repetition))
                directory.mkdir()
                exercise(directory, case)
    print('budget runner: 9 admission-boundary cases passed; no models or Docker')


if __name__ == '__main__':
    main()
