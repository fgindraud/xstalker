XStalker
========

Python daemon that listens to Xcb Randr events, and logs activity.
Tracks which window is focused, when, and make statistics.

Status
------

WIP.
Can retrieve some properties.

Install
-------

Requires:
* python >= 3.4
* xcffib python Xcb binding

Use standard distutils (--user will place it in a user local directory):

    python setup.py install [--user]

