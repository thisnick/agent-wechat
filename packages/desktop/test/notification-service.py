"""Freedesktop notification service for the isolated native-window acceptance."""
import json
import sys
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName("org.freedesktop.Notifications", bus)

class Notifications(dbus.service.Object):
    def __init__(self):
        super().__init__(bus, "/org/freedesktop/Notifications")
        self.next_id = 0

    @dbus.service.method("org.freedesktop.Notifications", out_signature="ssss")
    def GetServerInformation(self):
        return ("Acceptance fixture", "AgentWechat tests", "1", "1.2")

    @dbus.service.method("org.freedesktop.Notifications", out_signature="as")
    def GetCapabilities(self):
        return ["body", "actions", "sound"]

    @dbus.service.method("org.freedesktop.Notifications", in_signature="susssasa{sv}i", out_signature="u")
    def Notify(self, app, replaces, icon, title, body, actions, hints, expiry):
        self.next_id += 1
        with open(sys.argv[1], "a", encoding="utf-8") as output:
            output.write(json.dumps({"id": self.next_id, "title": str(title), "body": str(body), "soundFile": str(hints["sound-file"]) if "sound-file" in hints else None, "suppressSound": bool(hints.get("suppress-sound", False))}) + "\n")
        return self.next_id

    @dbus.service.method("org.freedesktop.Notifications", in_signature="u")
    def CloseNotification(self, notification_id):
        self.NotificationClosed(notification_id, 3)

    @dbus.service.signal("org.freedesktop.Notifications", signature="us")
    def ActionInvoked(self, notification_id, action):
        pass

    @dbus.service.signal("org.freedesktop.Notifications", signature="uu")
    def NotificationClosed(self, notification_id, reason):
        pass

    @dbus.service.method("io.github.thisnick.AgentWechat.Test", in_signature="u")
    def Click(self, notification_id):
        self.ActionInvoked(notification_id, "default")

service = Notifications()
print("Notification fixture ready", flush=True)
GLib.MainLoop().run()
