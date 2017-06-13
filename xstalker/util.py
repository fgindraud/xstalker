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

# Daemon

class Daemon (object):
    """
    Daemon objects that listen to file descriptors and can be activated when new data is available
    A daemon can ask to be reactivated immediately even if no new data is available.
    A counter ensure that reactivations does not loop undefinitely (it triggers an error).

    Must be implemented for each subclass :
        int fileno () : returns file descriptor, or None to be excluded (timeout only)
        int timeout () : timeout for object, in seconds, or None
        bool activate () : do stuff, and returns False to stop the event loop
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

                # Raise activation flag on all deamons with new input data
                # TODO handle timeout
                activated_daemons = selector_device.select ()
                for key, _ in activated_daemons:
                    key.fileobj._activation_reason = Daemon.ACTIVATED_DATA
        finally:
            selector_device.close ()

# Class introspection and pretty print

def class_attributes (cls):
    """ Return all class attributes (usually class constants) """
    return {attr: getattr (cls, attr) for attr in dir (cls) if not callable (attr) and not attr.startswith ("__")}

def sequence_stringify (iterable, highlight = lambda t: False, stringify = str):
    """ Print and join all elements of <iterable>, highlighting those matched by <highlight : obj -> bool> """
    def formatting (data):
        return ("[{}]" if highlight (data) else "{}").format (stringify (data))
    return " ".join (map (formatting, iterable))

