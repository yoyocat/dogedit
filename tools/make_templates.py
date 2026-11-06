#!/usr/bin/env python3
"""Write the bundled new-file templates.

`template.ini` is the `[Temp]`/`[TempN]` list the 新建 menu reads and the
模板 options page edits; each `File` names a starter document under
`template/`. A `Snippet=Y` entry is expanded through the snippet engine, so
`${1:…}` and `$0` become the first selection / final caret. The content here
is written for this project. Run from the repository root:

    python tools/make_templates.py
"""

import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEMPLATE_DIR = os.path.join(ROOT, "template")

# (title, type, file, snippet, body)
TEMPLATES = [
    ("HTML", "HTML", "template.htm", True, """\
<!DOCTYPE html>
<html lang="en">
<head>
\t<meta charset="utf-8">
\t<title>${1:New Document}</title>
</head>
<body>
\t$0
</body>
</html>
"""),
    ("PHP", "PHP", "template.php", True, """\
<?php
\t$0
?>
"""),
    ("Java", "Java", "template.java", True, """\
public class ${1:MyClass} {
\tpublic static void main(String[] args) {
\t\tSystem.out.println("${0:Hello, world}");
\t}
}
"""),
    ("Markdown", "Markdown", "", True, ""),
    ("C/C++", "C++", "template.cpp", True, """\
#include <stdio.h>

int main(int argc, char *argv[])
{
\tprintf("Hello, world\\n");
\t$0
\treturn 0;
}
"""),
]


def main():
    os.makedirs(TEMPLATE_DIR, exist_ok=True)
    lines = ["[Temp]", f"Count={len(TEMPLATES)}", "UseTempFormat=N"]
    for i, (title, type_, file, snippet, _) in enumerate(TEMPLATES):
        lines += [
            f"[Temp{i}]",
            f"Title={title}",
            f"Type={type_}",
            f"File={file}",
            f"Snippet={'Y' if snippet else 'N'}",
        ]
    with open(os.path.join(ROOT, "template.ini"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")
    for title, _, file, _, body in TEMPLATES:
        if not file:
            continue
        with open(os.path.join(TEMPLATE_DIR, file), "w", encoding="utf-8", newline="\n") as f:
            f.write(body)
    print(f"wrote template.ini and {sum(1 for t in TEMPLATES if t[2])} template files")


if __name__ == "__main__":
    main()
