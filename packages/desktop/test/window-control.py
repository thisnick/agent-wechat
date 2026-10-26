"""Native close/state checks inside the acceptance test's isolated Xvfb display."""
import ctypes
import ctypes.util
import json
import os
import re
import subprocess
import sys


action = sys.argv[1]
if action not in ("close", "state"):
    raise ValueError("Unknown window action")
info = subprocess.check_output(
    ["xwininfo", "-name", "AgentWechat"], env={**os.environ, "LC_ALL": "C"}, text=True
)
window = int(re.search(r"Window id: (0x[0-9a-fA-F]+)", info).group(1), 16)
if action == "close":
    x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x11.XInternAtom.restype = ctypes.c_ulong
    x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                              ctypes.c_long, ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]

    class ClientMessage(ctypes.Structure):
        _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong),
                    ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                    ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                    ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]

    class Event(ctypes.Union):
        _fields_ = [("client", ClientMessage), ("padding", ctypes.c_long * 24)]

    display = x11.XOpenDisplay(None)
    if not display:
        raise RuntimeError("Cannot open isolated display")
    try:
        event = Event()
        event.client.type = 33  # X11 ClientMessage
        event.client.display = display
        event.client.window = window
        event.client.message_type = x11.XInternAtom(display, b"WM_PROTOCOLS", 0)
        event.client.format = 32
        event.client.data[0] = x11.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
        if not x11.XSendEvent(display, window, 0, 0, ctypes.byref(event)):
            raise RuntimeError("Cannot request native window close")
    finally:
        x11.XCloseDisplay(display)

print(json.dumps({"visible": "Map State: IsViewable" in info}))
