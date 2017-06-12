# Copyright (c) 2013-2015 Francois GINDRAUD
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
XCB interface part of the daemon.
"""

import xcffib, xcffib.xproto

from . import util
logger = util.setup_logger (__name__)

class Backend (util.Daemon):
    ##################
    # Main Interface #
    ##################

    def __init__ (self, **kwd):
        """
        Backend init. Optionnal arguments :
            screen, display :
                override X11 default connect information
        """
        self.update_callback = (lambda _: 0)
        self.init_connection (**kwd)

    def cleanup (self):
        self.conn.disconnect ()
    
    def fileno (self):
        return self.conn.get_file_descriptor ()

    def activate (self):
        """ Daemon callback """
        self.handle_events ()
        return True # Tell event loop to continue

    def dump (self):
        """ Returns internal state debug info as a string """
        return "<TODO>"
    
    ############################
    # Layout Manager Interface # TODO resue for stat module
    ############################

    def attach (self, callback):
        """ Register the callback from the manager """
        self.update_callback = callback

    #################
    # X11 internals #
    #################

    def init_connection (self, **kwd):
        """ Starts connection, construct an initial state, setup events. """
        # Connection
        self.conn = xcffib.connect (display = kwd.get ("display"))

        # Internal state 
        screen_setup = self.conn.setup.roots[kwd.get ("screen", self.conn.pref_screen)]
        self.root = screen_setup.root

        # Randr register for events
        mask = xcffib.xproto.EventMask.FocusChange
        self.conn.core.ChangeWindowAttributes (self.root, xcffib.xproto.CW.EventMask, [mask], is_checked=True)
        self.conn.flush ()

    def handle_events (self):
        mode_flags_by_name = util.class_attributes (xcffib.xproto.NotifyMode)
        detail_flags_by_name = util.class_attributes (xcffib.xproto.NotifyDetail)
        def log_event (ev, name):
            logger.debug ("[notify] {} win={} mode=({}) detail=({})".format (
                name, ev.event, 
                util.sequence_stringify (mode_flags_by_name.items (),
                    highlight = lambda t: t[1] & ev.mode, stringify = lambda t: t[0]),
                util.sequence_stringify (detail_flags_by_name.items (),
                    highlight = lambda t: t[1] & ev.detail, stringify = lambda t: t[0])))
        
        ev = self.conn.poll_for_event ()
        while ev:
            # Detect if we received at least one randr event
            if isinstance (ev, xcffib.xproto.FocusInEvent):
                log_event (ev, "FocusInEvent")
            elif isinstance (ev, xcffib.xproto.FocusOutEvent):
                log_event (ev, "FocusOutEvent")
            ev = self.conn.poll_for_event ()

