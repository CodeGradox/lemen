"""Finder layout for dmgbuild; no custom image-building code."""

files = [defines["app"]]
symlinks = {"Applications": "/Applications"}
format = "UDZO"
background = "builtin-arrow"
window_rect = ((100, 100), (640, 280))
icon_locations = {"lemur.app": (140, 120), "Applications": (500, 120)}
icon_size = 96
text_size = 14
default_view = "icon-view"
show_status_bar = False
show_toolbar = False
show_sidebar = False
show_tab_view = False
