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

from . import util

logger = util.setup_logger (__name__)

class Context (object):
    def __init__ (self, **kwd):
        self.win_name = kwd.get ("win_name")
        self.win_class = kwd.get ("win_class")

def log_context (ctx):
    logger.debug ("[ctx] class='{}' name='{}'".format (ctx.win_class, ctx.win_name))

class StatManager (util.FixedIntervalTimeoutDaemon):
    def __init__ (self):
        super ().__init__ (5)

    def activate (self):
        assert self.activation_reason () == util.Daemon.ACTIVATED_TIMEOUT
        logger.debug ("[stats] timeout !")
