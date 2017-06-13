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

import pickle
import datetime

from . import util

logger = util.setup_logger (__name__)

class Context (object):
    def __init__ (self, **kwd):
        self.win_name = util.Optional (kwd.get ("win_name")).map (str.lower)
        self.win_class = util.Optional (kwd.get ("win_class")).map (str.lower)

class Database (object):
    version = 1
    """
    Format v1 is:
        * int : version number
    """
    def __init__ (self, db_file):
        # dict { (date, int (hour)) -> dict { category : str -> seconds : int } }
        self.db = {}

        # File
        self.db_file = db_file
        self.load_database ()

    # Manipulation

    def add_time_slice_for_category (self, category, from_ts, to_ts):
        logger.debug ("time slice for {}".format (category))
        # TODO cut time slice into seconds for time points (date x hour)

    def add_time_amount_for_category_at (self, category, seconds, time_point):
        # TODO create time_point entry if not there, add to category
        pass

    # Load / store database

    def store (self, buf):
        # Version
        pickle.dump (int (Database.version), buf)

    def load (self, buf):
        # Check version
        version = pickle.load (buf)
        if not isinstance (version, int):
            raise ValueError ("incorrect database format : version field = {}".format (version))
        if version != Database.version:
            raise ValueError ("incorrect database version : {} (expected {})".format (version, Database.version))

    # File versions of load / store

    def store_database (self):
        # Write to a temporary file
        temp_file = self.db_file.with_suffix (".temp")
        with temp_file.open ("wb") as db:
            self.store (db)

        # On success copy it to new position
        temp_file.rename (self.db_file)
        logger.info ("stored database into '{}'".format (self.db_file))

    def load_database (self):
        try:
            with self.db_file.open ("rb") as db:
                self.load (db)
                logger.info ("loaded database from '{}'".format (self.db_file))
        except FileNotFoundError:
            logger.warn ("database file '{}' not found".format (self.db_file))
        except Exception as e:
            logger.error ("unable to load database file '{}': {}".format (self.db_file, e))

class StatManager (util.FixedIntervalTimeoutDaemon):
    def __init__ (self, config):
        super ().__init__ (config["save_interval_sec"])
        
        self.db = Database (config["db_file"])
        self.filters = config["filters"]

        self.current_category = util.Optional ()
        self.current_category_since_ts = None

    def log (self, ctx):
        cat = self.determine_category (ctx)
        if self.current_category != cat:
            now = self.end_current_time_slice ()
            self.current_category.set_value (cat)
            self.current_category_since_ts = now
        logger.debug ("{} (class='{}' name='{}')".format (cat, ctx.win_class, ctx.win_name))

    def end_current_time_slice (self):
        # Returns now timestamp
        now = int (datetime.datetime.today ().timestamp ())
        if self.current_category:
            self.db.add_time_slice_for_category (self.current_category.value (), self.current_category_since_ts, now)
        return now

    def cleanup (self):
        self.end_current_time_slice ()
        self.db.store_database ()

    def determine_category (self, ctx):
        """
        Pick first matching category.
        self.filters format is list(("category_name", accept_func)).
        """
        for c, p in self.filters:
            if p (ctx):
                return c
        return None

    def activate (self):
        assert self.activation_reason () == util.Daemon.ACTIVATED_TIMEOUT
        self.db.store_database ()
