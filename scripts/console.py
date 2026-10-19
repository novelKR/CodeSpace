"""Terminal output for people: verdict words in color, a live status line while a long step runs,
and a plain copy of the output for a console log (docs/cs-rg-qualification.md#running).

Only what a person reads goes through here. Evidence, JSON, MCP messages, logs that a program
reads and anything a parent process captures are never styled. Redirected to a file or a pipe,
or with color off, every line is exactly the text a plain print writes; and without a terminal
and without a console log, nothing replaces stdout or stderr at all.

Color is chosen for each stream: `--color always` or `never`, or for `auto`, a terminal while
NO_COLOR is unset or empty (https://no-color.org) and TERM is not `dumb`. No environment
variable turns color on, so a child whose output is captured never inherits it.

Standard library only; Python 3.9 or later, as the qualification host's system Python."""
import contextlib
import os
import re
import sys
import threading
import time
import traceback

MODES = ('auto', 'always', 'never')
# Select Graphic Rendition parameters.
CODES = {'bold': '1', 'dim': '2', 'red': '31', 'green': '32', 'yellow': '33', 'magenta': '35', 'cyan': '36'}
ANSI = re.compile(r'\x1b\[[0-9;]*m')
# Each verdict word in the color of what its reader has to do: nothing (green), look at what failed
# (red), look at what could not be shown (yellow), or note that the run was stopped (magenta).
VERDICT_COLORS = {
    **dict.fromkeys(('pass', 'passed', 'qualified', 'verified', 'ok'), 'green'),
    **dict.fromkeys(('fail', 'failed', 'harness_failed'), 'red'),
    **dict.fromkeys(('inconclusive', 'incomplete', 'not_run'), 'yellow'),
    'interrupted': 'magenta'}
# How often the status line is redrawn.
TICK_S = 1.0


def add_option(parser):
    """`--color` for a script's command line."""
    parser.add_argument('--color', choices=MODES, default='auto',
                        help='color for people: auto (on a terminal, unless NO_COLOR is set), always or never')


def is_terminal(stream):
    try:
        return bool(stream.isatty())
    except (AttributeError, OSError, ValueError):
        return False


def wants_color(stream, mode='auto', environ=None):
    """Whether output to `stream` is colored under `mode`."""
    if mode not in MODES:
        raise ValueError(f'color is one of {", ".join(MODES)}, not {mode!r}')
    if mode != 'auto':
        return mode == 'always'
    environ = os.environ if environ is None else environ
    return not environ.get('NO_COLOR') and environ.get('TERM') != 'dumb' and is_terminal(stream)


def clock(seconds):
    """`seconds` as m:ss, or as h:mm:ss from an hour on."""
    seconds = max(0, int(seconds))
    hours, rest = divmod(seconds, 3600)
    minutes, seconds = divmod(rest, 60)
    return f'{hours}:{minutes:02d}:{seconds:02d}' if hours else f'{minutes}:{seconds:02d}'


class Style:
    """Text in SGR styles when on, and the text itself when off."""

    def __init__(self, on=False):
        self.on = bool(on)

    @classmethod
    def of(cls, stream, mode='auto', environ=None):
        return cls(wants_color(stream, mode, environ))

    def __call__(self, text, *names):
        text = str(text)
        if not (self.on and names and text):
            return text
        return '\x1b[' + ';'.join(CODES[name] for name in names) + 'm' + text + '\x1b[0m'

    def verdict(self, word):
        """A verdict word: bold, in the color of what its reader has to do."""
        color = VERDICT_COLORS.get(str(word))
        return self(word, 'bold', *((color,) if color else ()))

    def reasons(self, word, reasons):
        """A verdict's reasons as the harness prints them, a Python list: dim when there are
        none, and otherwise in the verdict's color."""
        color = VERDICT_COLORS.get(str(word))
        return self(reasons, 'dim') if not reasons else self(reasons, *((color,) if color else ()))


class Progress:
    """The status line of one step: its subject and how long it has run; with `planned_s`,
    `starting` until `begin` marks the start of the planned part, the time into it, and
    `concluding` once it is over; `detail`, a function, adds counts after a comma."""

    def __init__(self, subject, planned_s=None, detail=None):
        self.subject, self.planned_s, self.detail = subject, planned_s, detail
        self.created = time.monotonic()
        self.began = None

    def begin(self, at=None):
        """Mark the start of the planned part, now or at the monotonic time `at`."""
        self.began = time.monotonic() if at is None else at

    def __call__(self):
        now = time.monotonic()
        if self.planned_s is None:
            text = f'{self.subject} {clock(now - self.created)}'
        elif self.began is None or now < self.began:
            text = f'{self.subject} starting'
        elif now - self.began < self.planned_s:
            text = f'{self.subject} {clock(now - self.began)} of {clock(self.planned_s)}'
        else:
            text = f'{self.subject} concluding'
        extra = self.detail() if self.detail is not None else ''
        return f'{text}, {extra}' if extra else text


class Stream:
    """stdout or stderr during a session: what is written goes to the stream, after the status
    line is taken away, and a copy without colors goes to the console log."""

    def __init__(self, console, stream):
        self._console, self._stream = console, stream
        self._terminal = is_terminal(stream)

    def write(self, text):
        return self._console.write(self._stream, self._terminal, text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        self._stream.flush()

    def __getattr__(self, name):
        return getattr(self._stream, name)


class Console:
    """A process's output for people.

    `line` prints one line, styled with `style` (for stdout; `err_style` is for stderr). In a
    `session` on a terminal, `status` shows a live line under the output while a step runs,
    redrawn every second: every write to stdout or stderr takes it away first, and it comes back
    once the output is at a line start. A session's console log gets everything written to
    stdout and stderr, without colors and without the status line. Outside a session, or in one
    with neither a terminal nor a log, `line` is a plain print and nothing is replaced."""

    def __init__(self, name):
        self.name = name
        self.style = self.err_style = Style(False)
        self.lock = threading.RLock()
        self.terminal = self.log = self.ticker = None
        self.steps = []
        self.shown, self.drawn = 0, ''
        self.at_start = True
        self.stopped = threading.Event()

    def line(self, text='', err=False):
        with self.lock:
            print(text, file=sys.stderr if err else sys.stdout, flush=True)

    @contextlib.contextmanager
    def session(self, color='auto', log_path=None, live=True, environ=None):
        """For the block: color for stdout and stderr as `color` says, with `live` the status line
        when stdout is a terminal, and a new console log at `log_path`. An exception that leaves the
        block is copied to the log as the interpreter will print it."""
        environ = os.environ if environ is None else environ
        stdout, stderr = sys.stdout, sys.stderr
        self.style, self.err_style = Style.of(stdout, color, environ), Style.of(stderr, color, environ)
        terminal = stdout if live and is_terminal(stdout) and environ.get('TERM') != 'dumb' else None
        try:
            log = self.open_log(log_path) if log_path is not None else None
            if terminal is None and log is None:
                yield self
                return
            with self.lock:
                self.terminal, self.log, self.steps, self.at_start = terminal, log, [], True
                self.shown, self.drawn = 0, ''
                # Each session's status line has its own ticker, which ends with the session.
                self.stopped = threading.Event()
                sys.stdout, sys.stderr = Stream(self, stdout), Stream(self, stderr)
            try:
                yield self
            except BaseException as error:
                self.keep(error)
                raise
            finally:
                self.stopped.set()
                if self.ticker is not None:
                    self.ticker.join(timeout=5)
                with self.lock:
                    self.erase()
                    sys.stdout, sys.stderr = stdout, stderr
                    self.terminal = self.log = self.ticker = None
                    self.steps = []
                    if log is not None:
                        log.close()
        finally:
            self.style = self.err_style = Style(False)

    def open_log(self, path):
        """A new console log; an existing file is never written over."""
        try:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            raise SystemExit(f'{self.name}: {path} already exists') from None
        except OSError as error:
            raise SystemExit(f'{self.name}: the console log {path} cannot be created: {error}') from None
        return os.fdopen(descriptor, 'w', encoding='utf-8', errors='backslashreplace', newline='')

    def keep(self, error):
        """Copy to the log what the interpreter prints for an exception that ends the program."""
        if self.log is None:
            return
        if isinstance(error, SystemExit):
            text = '' if error.code is None or isinstance(error.code, int) else f'{error.code}\n'
        else:
            text = ''.join(traceback.format_exception(type(error), error, error.__traceback__))
        with self.lock:
            self.log.write(text)
            self.log.flush()

    @contextlib.contextmanager
    def status(self, text):
        """While the block runs, a terminal's status line shows `text`: a function, such as a
        Progress, or a subject that a Progress then times."""
        if self.terminal is None:
            yield
            return
        step = [text if callable(text) else Progress(text)]
        with self.lock:
            self.steps.append(step)
            if self.ticker is None:
                self.ticker = threading.Thread(target=self.tick, args=(self.stopped,), name='console-status',
                                               daemon=True)
                self.ticker.start()
        try:
            yield
        finally:
            with self.lock:
                self.steps = [entry for entry in self.steps if entry is not step]
                if not self.steps:
                    self.erase()

    def tick(self, stopped):
        while not stopped.wait(TICK_S):
            with self.lock:
                if not stopped.is_set() and self.terminal is not None and self.steps and self.at_start:
                    self.draw(self.steps[-1][0])

    def draw(self, text):
        try:
            text = str(text())
        except Exception:  # noqa: BLE001 - a status line never stops the work it shows
            return
        try:
            width = os.get_terminal_size(self.terminal.fileno()).columns
        except (AttributeError, OSError, ValueError):
            width = 0
        # A terminal whose size is unknown reports 0 columns.
        text = text[:(width or 80) - 1]
        if not text or (self.shown and text == self.drawn):
            return
        try:
            self.terminal.write('\r' + ' ' * self.shown + '\r' + self.style(text, 'dim'))
            self.terminal.flush()
        except (OSError, ValueError):
            return
        self.shown, self.drawn = len(text), text

    def erase(self):
        if self.shown and self.terminal is not None:
            try:
                self.terminal.write('\r' + ' ' * self.shown + '\r')
                self.terminal.flush()
            except (OSError, ValueError):
                pass
        self.shown, self.drawn = 0, ''

    def write(self, stream, terminal, text):
        with self.lock:
            if terminal:
                self.erase()
            count = stream.write(text)
            if text and terminal:
                self.at_start = text.endswith('\n')
            if self.log is not None and text:
                self.log.write(ANSI.sub('', text))
                if '\n' in text:
                    self.log.flush()
            return count
