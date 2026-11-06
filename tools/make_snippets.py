#!/usr/bin/env python3
"""Write the bundled snippet tables.

The format is the one `dg_core::SnippetFile::parse` reads: a `#title=` line,
then per snippet a `#t=<title>`, a `#g=<trigger>` and the body. The content
here is written for this project. Run from the repository root:

    python tools/make_snippets.py
"""

import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "app", "snippets")

# file: (title, [(snippet title, trigger, body), ...])
TABLES = {
    "c": ("C", [
        ("Main", "main", "int main(int argc, char **argv) {\n\t${1:return 0;}\n}"),
        ("For loop", "for", "for (int ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {\n\t${0:body}\n}"),
        ("If", "if", "if (${1:condition}) {\n\t${0:body}\n}"),
        ("Struct", "struct", "struct ${1:name} {\n\t${2:int field;};\n};"),
        ("Printf", "printf", "printf(\"${1:%s}\\n\", ${2:value});"),
        ("Include guard", "guard", "#ifndef ${1:NAME}_H\n#define ${1:NAME}_H\n\n${0:body}\n\n#endif"),
    ]),
    "py": ("Python", [
        ("Function", "def", "def ${1:name}($2):\n\t${0:pass}"),
        ("Class", "class", "class ${1:Name}(${2:object}):\n\tdef __init__(self$3):\n\t\t${0:pass}"),
        ("For loop", "for", "for ${1:item} in ${2:items}:\n\t${0:pass}"),
        ("If main", "ifmain", "if __name__ == '__main__':\n\t${1:main()}$0"),
        ("Try/except", "try", "try:\n\t${1:body}\n\texcept ${2:Exception} as exc:\n\t${0:raise}"),
        ("With open", "with", "with open(${1:path}, ${2:'r'}) as f:\n\t${0:data = f.read()}"),
    ]),
    "js": ("JavaScript", [
        ("Function", "fn", "function ${1:name}($2) {\n\t${0:return;}\n}"),
        ("Arrow", "arrow", "const ${1:name} = (${2:args}) => {\n\t${0:return;}\n};"),
        ("For of", "forof", "for (const ${1:item} of ${2:items}) {\n\t${0:body}\n}"),
        ("Console log", "log", "console.log(${1:value});"),
        ("Promise chain", "then", "${1:promise}\n\t.then((${2:value}) => ${3:value})\n\t.catch((${4:err}) => ${0:console.error(${4:err})});"),
    ]),
    "html": ("HTML", [
        ("Document", "html5", "<!DOCTYPE html>\n<html lang=\"${1:en}\">\n<head>\n\t<meta charset=\"utf-8\">\n\t<title>${2:Title}</title>\n</head>\n<body>\n\t${0}\n</body>\n</html>"),
        ("Division", "div", "<div class=\"${1:name}\">\n\t${0}\n</div>"),
        ("Link", "a", "<a href=\"${1:url}\">${2:text}</a>"),
        ("Script", "script", "<script src=\"${1:file.js}\"></script>"),
        ("Input", "input", "<input type=\"${1:text}\" name=\"${2:name}\" id=\"${2:name}\">"),
    ]),
    "css": ("CSS", [
        ("Rule", "rule", ".${1:selector} {\n\t${0:color: inherit;}\n}"),
        ("Flex row", "flex", "display: flex;\nalign-items: ${1:center};\njustify-content: ${2:space-between};"),
        ("Media query", "media", "@media (${1:min-width}: ${2:600px}) {\n\t${0}\n}"),
        ("Custom property", "var", ":root {\n\t--${1:name}: ${2:value};\n}"),
    ]),
    "java": ("Java", [
        ("Class", "class", "public class ${1:Name} {\n\t${0}\n}"),
        ("Main", "main", "public static void main(String[] args) {\n\t${0}\n}"),
        ("Print line", "sysout", "System.out.println(${1:value});"),
        ("For loop", "for", "for (int ${1:i} = 0; ${1:i} < ${2:n}; ${1:i}++) {\n\t${0}\n}"),
    ]),
    "lua": ("Lua", [
        ("Function", "func", "local function ${1:name}($2)\n\t${0}\nend"),
        ("For loop", "for", "for ${1:i} = 1, ${2:n} do\n\t${0}\nend"),
        ("If", "if", "if ${1:condition} then\n\t${0}\nend"),
        ("Table", "table", "local ${1:name} = {\n\t${2:key} = ${3:value},\n}"),
    ]),
    "bat": ("Batch", [
        ("Script header", "header", "@echo off\nsetlocal\n\n${0}\n\nendlocal"),
        ("For loop", "for", "for %%${1:i} in (${2:items}) do (\n\t${0:echo %%${1:i}}\n)"),
        ("If", "if", "if ${1:condition} (\n\t${0}\n)"),
        ("Variable", "set", "set \"${1:name}=${2:value}\""),
    ]),
    "ahk": ("AutoHotkey", [
        ("Hotkey", "hotkey", "#${1:^}!${2:key}::\n\t${0:return}"),
        ("Message box", "msgbox", "MsgBox, ${1:text}"),
        ("Loop", "loop", "Loop, ${1:count}\n{\n\t${0}\n}"),
        ("Function", "fn", "${1:Name}(${2:params})\n{\n\t${0}\n}"),
    ]),
    "perl": ("Perl", [
        ("Subroutine", "sub", "sub ${1:name} {\n\tmy (${2:@args}) = @_;\n\t${0}\n}"),
        ("Strict header", "strict", "use strict;\nuse warnings;\n\n${0}"),
        ("Foreach", "foreach", "foreach my ${1:$item} (${2:@items}) {\n\t${0}\n}"),
        ("Open file", "open", "open(my $fh, '<', ${1:$path}) or die \"$!\";"),
    ]),
    "php": ("PHP", [
        ("Block", "php", "<?php\n\t${0}\n?>"),
        ("Function", "fn", "function ${1:name}($2)\n{\n\t${0}\n}"),
        ("Foreach", "foreach", "foreach (${1:$items} as ${2:$item}) {\n\t${0}\n}"),
        ("Class", "class", "class ${1:Name}\n{\n\t${0}\n}"),
    ]),
    "vbs": ("VBScript", [
        ("Message box", "msgbox", "MsgBox \"${1:text}\""),
        ("Subroutine", "sub", "Sub ${1:Name}(${2:args})\n\t${0}\nEnd Sub"),
        ("For loop", "for", "For ${1:i} = ${2:1} To ${3:n}\n\t${0}\nNext"),
        ("If", "if", "If ${1:condition} Then\n\t${0}\nEnd If"),
    ]),
    "rails": ("Ruby", [
        ("Block", "block", "${1:items}.each do |${2:item}|\n\t${0}\nend"),
        ("Method", "def", "def ${1:name}($2)\n\t${0}\nend"),
        ("Class", "class", "class ${1:Name}\n\t${0}\nend"),
        ("Link", "link", "link_to ${1:'text'}, ${2:path}"),
    ]),
    "nsis": ("NSIS", [
        ("Section", "section", "Section \"${1:Name}\"\n\t${0}\nSectionEnd"),
        ("Set output", "outfile", "OutFile \"${1:installer.exe}\""),
        ("Page", "page", "Page directory\nPage instfiles"),
        ("File", "file", "File \"${1:path}\""),
    ]),
    "ansi": ("Control characters", [
        ("NUL", "nul", "ActiveDoc.Insert Chr(0)"),
        ("Bell", "bel", "ActiveDoc.Insert Chr(7)"),
        ("Escape", "esc", "ActiveDoc.Insert Chr(27)"),
        ("Line feed", "lf", "ActiveDoc.Insert Chr(10)"),
        ("Carriage return", "cr", "ActiveDoc.Insert Chr(13)"),
        ("Tab", "tab", "ActiveDoc.Insert Chr(9)"),
    ]),
    "html_entity": ("HTML entities", [
        ("Ampersand", "amp", "&amp;"),
        ("Less than", "lt", "&lt;"),
        ("Greater than", "gt", "&gt;"),
        ("Non-breaking space", "nbsp", "&nbsp;"),
        ("Quote", "quot", "&quot;"),
        ("Copyright", "copy", "&copy;"),
    ]),
}


def render(title, rows):
    lines = ["#title=" + title, ""]
    for t, g, body in rows:
        lines.append("#t=" + t)
        lines.append("#g=" + g)
        lines.extend(body.split("\n"))
        lines.append("")
    return "\n".join(lines)


def main():
    for name, (title, rows) in TABLES.items():
        with open(os.path.join(OUT, name + ".snippet"), "w", newline="\n", encoding="utf-8") as f:
            f.write(render(title, rows))
    print("wrote %d snippet tables" % len(TABLES))


if __name__ == "__main__":
    main()
