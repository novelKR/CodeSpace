"""Terminal output for people (scripts/console.py): color only where it is asked for or a terminal
shows it, a status line only on a terminal, and a console log that is the plain text."""
import contextlib
import io
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import console  # noqa: E402

PLAIN = {'PATH': '/usr/bin'}


class Terminal(io.StringIO):
    """A stream that says it is a terminal and has no descriptor, so the width is the fallback."""

    def isatty(self):
        return True


def until(condition, limit=5.0):
    deadline = time.monotonic() + limit
    while not condition():
        if time.monotonic() > deadline:
            raise AssertionError('the condition did not hold in time')
        time.sleep(0.01)


class ColorTests(unittest.TestCase):
    def test_auto_colors_only_a_terminal(self):
        self.assertTrue(console.wants_color(Terminal(), 'auto', PLAIN))
        self.assertFalse(console.wants_color(io.StringIO(), 'auto', PLAIN))
        self.assertFalse(console.wants_color(Terminal(), 'auto', dict(PLAIN, NO_COLOR='1')))
        self.assertTrue(console.wants_color(Terminal(), 'auto', dict(PLAIN, NO_COLOR='')))
        self.assertFalse(console.wants_color(Terminal(), 'auto', dict(PLAIN, TERM='dumb')))
        self.assertTrue(console.wants_color(io.StringIO(), 'always', dict(PLAIN, NO_COLOR='1')))
        self.assertFalse(console.wants_color(Terminal(), 'never', PLAIN))
        with self.assertRaises(ValueError):
            console.wants_color(Terminal(), 'sometimes', PLAIN)

    def test_a_style_that_is_off_is_the_text(self):
        style = console.Style(False)
        self.assertEqual(style('qualify:', 'dim'), 'qualify:')
        self.assertEqual(style.verdict('qualified'), 'qualified')
        self.assertEqual(style.reasons('inconclusive', ['a reason']), "['a reason']")
        self.assertEqual(style.reasons('pass', []), '[]')

    def test_verdict_colors(self):
        style = console.Style(True)
        self.assertEqual(style.verdict('pass'), '\x1b[1;32mpass\x1b[0m')
        self.assertEqual(style.verdict('qualified'), '\x1b[1;32mqualified\x1b[0m')
        self.assertEqual(style.verdict('harness_failed'), '\x1b[1;31mharness_failed\x1b[0m')
        self.assertEqual(style.verdict('not_run'), '\x1b[1;33mnot_run\x1b[0m')
        self.assertEqual(style.verdict('interrupted'), '\x1b[1;35minterrupted\x1b[0m')
        self.assertEqual(style.verdict('unknown'), '\x1b[1munknown\x1b[0m')
        self.assertEqual(style.reasons('pass', []), '\x1b[2m[]\x1b[0m')
        self.assertEqual(style.reasons('fail', ['x']), "\x1b[31m['x']\x1b[0m")
        self.assertEqual(style('', 'bold'), '')
        self.assertEqual(console.ANSI.sub('', style.verdict('fail') + ' ' + style('x', 'dim')), 'fail x')


class ProgressTests(unittest.TestCase):
    def test_clock(self):
        self.assertEqual([console.clock(s) for s in (-1, 0, 59.9, 61, 3599, 3600, 36061)],
                         ['0:00', '0:00', '0:59', '1:01', '59:59', '1:00:00', '10:01:01'])

    def test_progress_text(self):
        now = [100.0]
        with mock.patch.object(console.time, 'monotonic', lambda: now[0]):
            plain = console.Progress('qualify: matrix')
            interval = console.Progress('qualify: uds-1 load:', 1800, lambda: '29 load runs')
            now[0] = 165.0
            self.assertEqual(plain(), 'qualify: matrix 1:05')
            self.assertEqual(interval(), 'qualify: uds-1 load: starting, 29 load runs')
            interval.begin(170.0)
            self.assertEqual(interval(), 'qualify: uds-1 load: starting, 29 load runs')
            now[0] = 893.0
            self.assertEqual(interval(), 'qualify: uds-1 load: 12:03 of 30:00, 29 load runs')
            now[0] = 1970.0
            interval.detail = lambda: ''
            self.assertEqual(interval(), 'qualify: uds-1 load: concluding')


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)
        self.console = console.Console('test')

    def tearDown(self):
        self.scratch.cleanup()

    def test_without_a_terminal_or_a_log_nothing_is_replaced(self):
        with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()):
            with self.console.session('auto', environ=PLAIN):
                self.assertIs(sys.stdout, out)
                with self.console.status('a step'):
                    self.console.line(self.console.style.verdict('passed'))
            self.assertIs(sys.stdout, out)
        self.assertEqual(out.getvalue(), 'passed\n')
        self.assertFalse(self.console.style.on)

    def test_the_console_log_is_the_plain_output(self):
        log = self.dir / 'run.console.log'
        with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
            with self.console.session('always', log, environ=PLAIN):
                self.console.line(f"{self.console.style('qualify:', 'dim')} uds-1 load: "
                                  f"{self.console.style.verdict('pass')}")
                print('measure: a line of its own', flush=True)
                print('a warning', file=sys.stderr)
            self.assertIs(sys.stdout, out)
        self.assertIn('\x1b[1;32mpass\x1b[0m', out.getvalue())
        self.assertEqual(console.ANSI.sub('', out.getvalue()), 'qualify: uds-1 load: pass\nmeasure: a line of its own\n')
        self.assertEqual(err.getvalue(), 'a warning\n')
        self.assertEqual(log.read_text(), 'qualify: uds-1 load: pass\nmeasure: a line of its own\na warning\n')
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)

    def test_a_console_log_is_never_written_over(self):
        log = self.dir / 'run.console.log'
        log.write_text('an earlier run\n')
        with self.assertRaises(SystemExit) as raised:
            with self.console.session('never', log, environ=PLAIN):
                self.fail('the session began')
        self.assertEqual(str(raised.exception.code), f'test: {log} already exists')
        self.assertEqual(log.read_text(), 'an earlier run\n')
        with self.assertRaises(SystemExit) as raised:
            with self.console.session('never', self.dir / 'missing' / 'run.console.log', environ=PLAIN):
                self.fail('the session began')
        self.assertIn('cannot be created', str(raised.exception.code))

    def test_what_ends_the_program_is_kept_in_the_log(self):
        log = self.dir / 'exit.console.log'
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit):
            with self.console.session('never', log, environ=PLAIN):
                raise SystemExit('test: the evidence directory already exists')
        self.assertEqual(log.read_text(), 'test: the evidence directory already exists\n')
        log = self.dir / 'trace.console.log'
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(KeyError):
            with self.console.session('never', log, environ=PLAIN):
                raise KeyError('missing')
        text = log.read_text()
        self.assertTrue(text.startswith('Traceback (most recent call last):\n'), text)
        self.assertTrue(text.endswith("KeyError: 'missing'\n"), text)
        log = self.dir / 'status.console.log'
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit):
            with self.console.session('never', log, environ=PLAIN):
                raise SystemExit(2)
        self.assertEqual(log.read_text(), '')

    def test_a_terminal_shows_the_status_line_and_the_log_does_not(self):
        screen, log = Terminal(), self.dir / 'live.console.log'
        with mock.patch.object(console, 'TICK_S', 0.01), contextlib.redirect_stdout(screen):
            with self.console.session('never', log, environ=PLAIN):
                with self.console.status(console.Progress('qualify: uds-1 idle:', 600)):
                    until(lambda: self.console.shown)
                    self.console.line('qualify: uds pipe exits: pass')
                    until(lambda: self.console.shown)
                self.assertEqual(self.console.shown, 0)
                with self.console.status('qualify: recovery'):
                    until(lambda: self.console.shown)
                    print('qualify: a partial', end='', flush=True)
                    self.assertEqual(self.console.shown, 0)
                    time.sleep(0.05)
                    self.assertEqual(self.console.shown, 0)
                    print(' line', flush=True)
                    until(lambda: self.console.shown)
            self.assertIs(sys.stdout, screen)
        text = screen.getvalue()
        status = 'qualify: uds-1 idle: starting'
        self.assertTrue(text.startswith('\r\r' + status), repr(text[:80]))
        self.assertIn('\r' + ' ' * len(status) + '\rqualify: uds pipe exits: pass\n', text)
        self.assertIn('qualify: a partial line\n', text)
        self.assertIn('\rqualify: recovery 0:00', text)
        self.assertTrue(text.endswith('\r' + ' ' * len('qualify: recovery 0:00') + '\r'), repr(text[-80:]))
        self.assertEqual(log.read_text(), 'qualify: uds pipe exits: pass\nqualify: a partial line\n')

    def test_a_status_line_fits_the_terminal_and_is_dim_in_color(self):
        screen = Terminal()
        with mock.patch.object(console, 'TICK_S', 0.01), contextlib.redirect_stdout(screen):
            with self.console.session('always', environ=PLAIN):
                with self.console.status(lambda: 'x' * 200):
                    until(lambda: self.console.shown)
        self.assertIn('\r\x1b[2m' + 'x' * 79 + '\x1b[0m', screen.getvalue())
        self.assertNotIn('x' * 80, screen.getvalue())

    def test_an_unchanged_status_is_drawn_once(self):
        screen = Terminal()
        with mock.patch.object(console, 'TICK_S', 0.01), contextlib.redirect_stdout(screen):
            with self.console.session('never', environ=PLAIN):
                with self.console.status(lambda: 'qualify: uds-1 load: concluding'):
                    until(lambda: self.console.shown)
                    time.sleep(0.1)
        status = 'qualify: uds-1 load: concluding'
        self.assertEqual(screen.getvalue(), '\r\r' + status + '\r' + ' ' * len(status) + '\r')

    def test_a_broken_status_never_stops_the_work(self):
        screen = Terminal()

        def broken():
            raise RuntimeError('no counts yet')

        with mock.patch.object(console, 'TICK_S', 0.01), contextlib.redirect_stdout(screen):
            with self.console.session('never', environ=PLAIN):
                with self.console.status(broken):
                    time.sleep(0.05)
                    self.console.line('qualify: still running')
        self.assertEqual(screen.getvalue(), 'qualify: still running\n')

    def test_a_dumb_terminal_or_no_live_output_has_no_status_line(self):
        for environ, live in ((dict(PLAIN, TERM='dumb'), True), (PLAIN, False)):
            screen = Terminal()
            with mock.patch.object(console, 'TICK_S', 0.01), contextlib.redirect_stdout(screen):
                with self.console.session('auto', live=live, environ=environ):
                    self.assertIs(sys.stdout, screen)
                    with self.console.status('qualify: matrix'):
                        time.sleep(0.05)
                        self.console.line(self.console.style.verdict('passed'))
            self.assertEqual(screen.getvalue(), 'passed\n' if environ.get('TERM') else '\x1b[1;32mpassed\x1b[0m\n')


if __name__ == '__main__':
    unittest.main()
