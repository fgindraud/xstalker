XStalker
========

Rust daemon that listens to Xcb events, and logs activity.
Tracks which window is focused, when, and make statistics.

Status
------

WIP.
* Retrieves `WM_NAME` and `WM_CLASS`
* TODO:
	* More context: cwd of pid ?
	* Update on `WM_*` change on active window ?

Install
-------

Requires: Rust, xcb.

