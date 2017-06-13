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
XCB interface part of the daemon.
"""

import xcffib
import xcffib.xproto
import struct

from . import util
from . import stats

logger = util.setup_logger (__name__)

class Backend (util.Daemon):
    ##################
    # Main Interface #
    ##################

    # __init__ API
    def __init__ (self, **kwd):
        """
        Backend init. Optionnal arguments :
            screen, display :
                override X11 default connect information
        """
        super ().__init__ ()
        self.callback = (lambda _: 0)
        self.init_connection (**kwd)

    def cleanup (self):
        self.conn.disconnect ()
    
    def dump (self):
        """ Returns internal state debug info as a string """
        return "<TODO>"
    
    # Daemon API
    def fileno (self):
        return self.conn.get_file_descriptor ()

    def activate (self):
        """ Daemon callback """
        self.handle_events ()
        return True # Tell event loop to continue

    ############################
    # Layout Manager Interface # TODO resue for stat module
    ############################

    def attach (self, callback):
        """ Register the callback from the manager """
        self.callback = callback
        self.active_window_changed () # Force reloading state and call callback

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

        # Track changes in _NET_ACTIVE_WINDOW on root window (indicates which window has focus)
        # This rely on the WM to have extended WM hints support, but most do

        # Get useful atoms
        self.active_window_atom = self.get_custom_atom ("_NET_ACTIVE_WINDOW")
        self.utf8_string_atom = self.get_custom_atom ("UTF8_STRING")
        self.compound_text_atom = self.get_custom_atom ("COMPOUND_TEXT")

        # Get Property events on root
        mask = xcffib.xproto.EventMask.PropertyChange
        self.conn.core.ChangeWindowAttributes (self.root, xcffib.xproto.CW.EventMask, [mask], is_checked=True)
        self.conn.flush ()

    def get_custom_atom (self, name):
        return self.conn.core.InternAtom (True, len (name), name).reply ().atom

    def get_string_property (self, win_id, atom):
        # Send // requests
        req = self.conn.core.GetProperty (False, win_id, atom, xcffib.xproto.Atom.STRING, 0, 400)
        utf8_req = self.conn.core.GetProperty (False, win_id, atom, self.utf8_string_atom, 0, 400)
        ct_req = self.conn.core.GetProperty (False, win_id, atom, self.compound_text_atom, 0, 400)
        # Replies (failure is considered no-value)
        get_reply = lambda r: r.reply ()
        reply = util.Optional (req).map_with_error (get_reply, xcffib.Error)
        utf8_reply = util.Optional (utf8_req).map_with_error (get_reply, xcffib.Error)
        ct_reply = util.Optional (ct_req).map_with_error (get_reply, xcffib.Error)
        # Parse replies
        def check_reply (atom):
            return lambda r: r.format == 8 and r.type == atom and r.bytes_after == 0
        return reply.filter (check_reply (xcffib.xproto.Atom.STRING)).map (lambda v: v.value.to_string ()) | \
                utf8_reply.filter (check_reply (self.utf8_string_atom)).map (lambda v: v.value.to_utf8 ()) | \
                ct_reply.filter (check_reply (self.compound_text_atom)).map (lambda v: v.value.to_utf8 ())
    
    def get_window_name (self, win_id):
        return self.get_string_property (win_id, xcffib.xproto.Atom.WM_NAME)

    def get_window_class (self, win_id):
        get_class_name = lambda s: s.split ('\x00')[0] # has 2 '\0'-separated strings
        return self.get_string_property (win_id, xcffib.xproto.Atom.WM_CLASS).map (get_class_name)

    def get_active_window_id (self):
        data = self.conn.core.GetProperty (
                False, # Do not delete prop
                self.root,
                self.active_window_atom,
                xcffib.xproto.Atom.WINDOW,
                0, 100).reply ()
        if not (data.format > 0 and data.type == xcffib.xproto.Atom.WINDOW and
                data.bytes_after == 0 and data.length == 1):
            raise Exception ("invalid window id formatting")
        (active_win_id,) = struct.unpack_from ({ 8: "b", 16: "h", 32: "i" }[data.format], data.value.buf ())
        return active_win_id

    def active_window_changed (self):
        # _NET_ACTIVE_WINDOW changed on root window, get new value
        active_win_id = self.get_active_window_id ()
        active_win_name = self.get_window_name (active_win_id)
        active_win_class = self.get_window_class (active_win_id)
        self.callback (stats.Context (win_name = active_win_name, win_class = active_win_class))

    def handle_events (self):
        ev = self.conn.poll_for_event ()
        while ev:
            if isinstance (ev, xcffib.xproto.PropertyNotifyEvent) and ev.window == self.root and \
                    ev.state == xcffib.xproto.Property.NewValue and ev.atom == self.active_window_atom:
                self.active_window_changed ()
            ev = self.conn.poll_for_event ()

