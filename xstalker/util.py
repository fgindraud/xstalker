# Copyright (c) 2013-2017 Francois GINDRAUD
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

import select
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
    A counter ensure that reactivations does not loop undefinitely.

    Must be implemented for each subclass :
        int fileno () : returns file descriptor
        bool activate () : do stuff, and returns False to stop the event loop
    """
    def activate_manually (self):
        """ Ask the event loop to activate us again """
        self._flag_to_be_activated = True

    def _to_be_activated (self):
        # Try-except handles the init case, where the flag doesn't exist
        try:
            return self._flag_to_be_activated
        except AttributeError:
            return False

    def _reset_activation_counter (self):
        self._activation_counter = 0
    
    def _activate (self):
        # If activation counter doesn't exist, we are not in an event loop and we don't care
        try:
            self._activation_counter += 1
            if self._activation_counter > 10:
                raise RuntimeError ("daemon reactivation loop detected")
        except AttributeError:
            pass
        return self.activate ()

    @staticmethod
    def event_loop (*daemons):
        # Quit nicely on SIGTERM
        import signal
        def sigterm_handler (sig, stack):
            import sys
            sys.exit ()
        signal.signal (signal.SIGTERM, sigterm_handler)
        # Event loop itself
        while True:
            # Activate deamons until no one has the activation flag raised
            map (Daemon._reset_activation_counter, daemons)
            while any (map (Daemon._to_be_activated, daemons)):
                d = next (filter (Daemon._to_be_activated, daemons))
                d._flag_to_be_activated = False
                if d._activate () == False:
                    return

            # Raise activation flag on all deamons with new input data
            new_data, _, _ = select.select (daemons, [], [])
            for d in new_data:
                d._flag_to_be_activated = True

# Class introspection and pretty print

def class_attributes (cls):
    """ Return all class attributes (usually class constants) """
    return {attr: getattr (cls, attr) for attr in dir (cls) if not callable (attr) and not attr.startswith ("__")}

def sequence_stringify (iterable, highlight = lambda t: False, stringify = str):
    """ Print and join all elements of <iterable>, highlighting those matched by <highlight : obj -> bool> """
    def formatting (data):
        return ("[{}]" if highlight (data) else "{}").format (stringify (data))
    return " ".join (map (formatting, iterable))

