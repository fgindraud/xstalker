XStalker
========

Rust daemon that listens to Xcb events, and logs activity.
Tracks which window is focused, when, and make statistics.


Status
------

WIP.
* Retrieves `WM_NAME` and `WM_CLASS`
* TODO:
	* Update to futures 0.3 (seems to be evolution of tokio)
	* Revamp database system : dynamically discover categories. Remove static list of categories.
	* Process: remove fields header from input ?
	* More context: cwd of pid ?
	* Update on `WM_*` change on active window ?

Install
-------

Requires: Rust, xcb.

