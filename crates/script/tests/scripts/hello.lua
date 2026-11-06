-- M1: 引擎与宿主联通 — App.alert / App.log / App.version
App.alert(("hello from dg-script %s (%s), doc %q has %d lines"):format(
    App.version(), _VERSION, App.doc.name(), App.doc.line_count()))
App.log("hello.lua finished")
