# dmgbuild settings for Berth's disk image: the app and an Applications link
# side by side on packaging/dmg/background.svg. scripts/dmg.sh passes
# -D app=<Berth.app> -D background=<tiff> -D icon=<icns>.
# `defines` is provided by dmgbuild.
import os.path

app = defines["app"]  # noqa: F821
app_name = os.path.basename(app)

format = "ULMO"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]  # noqa: F821
background = defines["background"]  # noqa: F821

# The window: 660x400 points, nothing but the two icons.
window_rect = ((200, 120), (660, 400))
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
default_view = "icon-view"
show_icon_preview = False
arrange_by = None
label_pos = "bottom"
text_size = 13
icon_size = 128
icon_locations = {app_name: (170, 180), "Applications": (490, 180)}
# No `hide_extensions`: its Finder flag on the bundle breaks the app's
# signature check (Finder hides ".app" anyway).
