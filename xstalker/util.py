# Copyright (c) 2017 Francois GINDRAUD
# 
# Permission is hereby granted, free of charge, to any person obtaining
# a copy of this software and associated documentation files (the
# "Software"), to deal in the Software without restriction, including
# without limitation the rights to use, copy, modify, merge, publish,
# distribute, sublicense, and/or sell copies of the Software, and to
# permit persons to whom the Software is furnished to do so, subject to
# the following conditions:
# 
# The above copyright notice and this permission notice shall be
# included in all copies or substantial portions of the Software.
# 
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
# EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
# MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
# NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
# LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
# OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
# WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

"""
Utilities
"""

import logging, logging.handlers

# Logging

def setup_root_logging (filename, level):
    root = logging.getLogger ()
    root.setLevel (level)
    formatter = logging.Formatter (style = "{", fmt = "{asctime} :: {levelname} :: {name} :: {message}")
    
    if filename:
        output = logging.handlers.RotatingFileHandler (filename, "a", 1000000, 1)
    else:
        output = logging.StreamHandler ()
    
    output.setLevel (level)
    output.setFormatter (formatter)
    root.addHandler (output)

    return root

def setup_logger (module_name):
    return logging.getLogger (module_name)

logger = setup_logger (__name__)

# Optional

class Optional (object):
    def __init__ (self, obj = None):
        self.obj = obj
        while isinstance (self.obj, Optional):
            self.obj = self.obj.obj
    def has_value (self):
        return self.obj is not None
    def __bool__ (self):
        return self.has_value ()
    def value (self):
        assert self.has_value ()
        return self.obj
    def set_value (self, val):
        self.obj = val
    def map (self, f):
        """ Returns an Optional with f(self) if has_value, or empty Optional """
        return Optional (f (self.value ()) if self.has_value () else None)
    def map_with_error (self, f, exc_type=Exception):
        """ Tries to perform map(f), returns empty Optional on exception """
        try:
            return self.map (f)
        except exc_type:
            return Optional ()
    def filter (self, p):
        """ Propgate self only if has a value and p(value) is True ; returns empty Optional otherwise """
        return self if self.has_value () and p (self.value ()) else Optional ()
    def __or__ (self, other):
        """ Returns self if has_value, or the other element """
        return self if self else other
    def __str__ (self):
        return str (self.obj)
    def __repr__ (self):
        return "Optional({})".format (repr (self.obj))
    def __eq__ (self, other):
        if isinstance (other, Optional) and not self.has_value () and not other.has_value ():
            return True # Empty Optionals are equal
        return self.has_value () and other == self.value ()

# Daemon

class Daemon (object):
    """
    Daemon objects are objects that can activated when some conditions happen in an event_loop.
    They can be activated if:

    1/ New data is available on a file descriptor.
    To enable this behavior, fileno() must return a descriptor integer instead of None
    This integer must be constant for the event_loop.

    2/ The wait in the event_loop timeouts.
    To enable this, timeout() must return an integer >= 0 (in seconds) instead of None

    3/ The daemon is activated manually.
    During execution, some code calls d.activate_manually() on the daemon to make it activate.
    This is useful to reactivate a daemon event if no new data is available.
    A counter ensure that reactivations does not loop undefinitely (it triggers an error).

    Finally, an activate() callback function must be implemented.
    It must return a bool indicating if the event loop should continue.
    During its execution, d.activation_reason() gives the reason for activation.
    """

    NOT_ACTIVATED = 0
    ACTIVATED_MANUAL = 1
    ACTIVATED_TIMEOUT = 2
    ACTIVATED_DATA = 3

    # Default version of API
    def fileno (self):
        return None
    def timeout (self):
        return None
    def activate (self):
        raise NotImplementedError

    # Methods provided to subclasses
    def __init__ (self):
        """ Creates internal variables """
        self._activation_reason = self.NOT_ACTIVATED
        self._current_activation_reason = self.NOT_ACTIVATED
        self._activation_counter = 0

    def activate_manually (self):
        """ Ask the event loop to activate us again """
        self._activation_reason = self.ACTIVATED_MANUAL

    def activation_reason (self):
        """ Gives us the activation reason for this call of activate() """
        return self._current_activation_reason

    # Internal stuff
    def _is_activated (self):
        return self._activation_reason != self.NOT_ACTIVATED
    def _activate (self):
        # Detect possible activate_manually () loop
        self._activation_counter += 1
        if self._activation_counter > 100:
            raise RuntimeError ("Daemon.event_loop: reactivation loop detected")
        # Set context for activate (), then clean
        self._current_activation_reason = self._activation_reason
        self._activation_reason = self.NOT_ACTIVATED
        continue_event_loop = self.activate ()
        self._current_activation_reason = self.NOT_ACTIVATED
        return continue_event_loop

    # Top level event_loop system
    @staticmethod
    def event_loop (*daemons):
        """
        Take a list of daemons as input, handle their activation in an event loop.
        fileno(): is supposed constant (only read once).
        timeout(): read at each cycle ; only the smallest timeout daemon is activated for timeout.
        """
        # Quit nicely on SIGTERM
        import signal
        def sigterm_handler (sig, stack):
            import sys
            sys.exit ()
        signal.signal (signal.SIGTERM, sigterm_handler)

        # Event loop setup : use selector library
        import selectors
        selector_device = selectors.DefaultSelector ()
        try:
            for d in daemons:
                if d.fileno () is not None:
                    selector_device.register (d, selectors.EVENT_READ)

            while True:
                # Activate deamons until no one has the activation flag raised
                for d in daemons:
                    d._activation_counter = 0
                while any (map (Daemon._is_activated, daemons)):
                    d = next (filter (Daemon._is_activated, daemons))
                    if d._activate () == False:
                        return

                # First determine if a timeout is used, and which daemons will timeout first
                timeout = None
                lowest_timeout_daemons = []
                for d, t in ((d, d.timeout()) for d in daemons):
                    if t is not None:
                        if timeout is None or t < timeout:
                            timeout = t
                            lowest_timeout_daemons = [d]
                        elif t == timeout:
                            lowest_timeout_daemons.append (d)
                # Check for input data using select
                activated_daemons = selector_device.select (timeout)
                if len (activated_daemons) > 0:
                    for key, _ in activated_daemons:
                        key.fileobj._activation_reason = Daemon.ACTIVATED_DATA
                else:
                    # Timeout
                    for d in lowest_timeout_daemons:
                        d._activation_reason = Daemon.ACTIVATED_TIMEOUT
        finally:
            selector_device.close ()

class FixedIntervalTimeoutDaemon (Daemon):
    """
    Partial impl class to activate at a fixed interval in seconds (not precise)
    """
    def __init__ (self, interval_sec):
        super ().__init__ ()
        self._interval_sec = interval_sec
        self._next_timeout_timestamp_sec = self._now () + interval_sec

    def _now (self):
        import time
        return int (time.time ())

    def timeout (self):
        remaining = self._next_timeout_timestamp_sec - self._now ()
        return max (0, min (remaining, self._interval_sec)) # Clamp in case of clock shift

    def _activate (self):
        self._next_timeout_timestamp_sec = self._now () + self._interval_sec
        super ()._activate ()

# Class introspection and pretty print

def class_attributes (cls):
    """ Return all class attributes (usually class constants) """
    return {attr: getattr (cls, attr) for attr in dir (cls) if not callable (attr) and not attr.startswith ("__")}

def sequence_stringify (iterable, highlight = lambda t: False, stringify = str):
    """ Print and join all elements of <iterable>, highlighting those matched by <highlight : obj -> bool> """
    def formatting (data):
        return ("[{}]" if highlight (data) else "{}").format (stringify (data))
    return " ".join (map (formatting, iterable))

