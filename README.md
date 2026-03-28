# HackArena CLI - Participant Guide

`hackarena` is the official CLI for HackArena 3 projects:
- installs `ha-auth` globally,
- installs local `backend` and `wrappers/`,
- updates components from GitHub Releases,
- sends submissions (`hackarena submit`).

## 1. Requirements

- Internet access.
- Tools depending on your wrapper:
  - Python: `python`, `pip` (preferably inside `.venv`),
  - C#: `.NET SDK`,
  - TypeScript: `node` + `npm`,
  - C++: `cmake` + compiler; on Windows only MSVC (Visual Studio / `cl.exe`).

## 2. First run

In your project directory:

```bash
hackarena use 3
hackarena install
```

This will:
- create/update `.hackarena/project.json`,
- install global `ha-auth`,
- install local backend (`./backend`),
- install a wrapper (interactive when needed).

Authentication:

```bash
hackarena auth login
hackarena auth whoami
```

## 3. Most important commands

```bash
hackarena status
hackarena doctor
hackarena update
hackarena update wrapper <wrapper_id>
hackarena install wrapper
hackarena install wrapper <python|csharp|cpp|typescript>
hackarena clean
```

Notes:
- `install` does not overwrite an already valid wrapper installation.
- use `update` to upgrade versions.

## 4. Wrapper layout and local run

After installation, wrapper files are in:

```text
wrappers/<id>/
  system/manifest.toml
  user/...
```

You can install multiple instances of the same wrapper (`python`, `python_1`, `python_2`, ...).

### Python

```bash
python -m pip install -r user/requirements.txt
```

### C#

```bash
dotnet run --project user/Bot.csproj
```

### TypeScript

```bash
npm install
```

### C++

Windows (MSVC):

```bash
cmake -S user -B user/build -G "Visual Studio 17 2022" -A x64
cmake --build user/build --config Release
```

Linux/macOS:

```bash
cmake -S user -B user/build -DCMAKE_BUILD_TYPE=Release
cmake --build user/build
```

## 5. Submit

Submit requires logged-in `ha-auth` and (for edition 3) mandatory `--slot`:

```bash
hackarena submit --slot 1
hackarena submit --slot 2 -d "my attempt"
```

If multiple wrappers exist, CLI will show a numbered list and ask for selection.

Submit manifest is read only from:

```text
wrappers/<id>/system/manifest.toml
```

### TypeScript - important

`[submit].include` must contain `user/package.json` and files from `user/src`, for example:

```toml
[submit]
include = ["user/package.json", "user/src/**/*"]
exclude = ["user/node_modules/**/*", "user/dist/**/*", "user/build/**/*"]
```

## 6. GitHub token (private repos)

PowerShell:

```powershell
$env:GH_TOKEN = "ghp_xxx"
```

bash/zsh:

```bash
export GH_TOKEN=ghp_xxx
```

If the token does not have repository access, GitHub API often returns `404`.

## 7. Linux ABI (Linux only)

`install` and `update` support:

```bash
--linux-libc auto|gnu|musl
```

Default is `auto` (with fallback logic).

## 8. Quick troubleshooting

- `No project found` / missing `.hackarena/project.json`:
  - run `hackarena use 3`.
- `GitHub API returned 404`:
  - check owner/repo and token permissions.
- C++ build fails on Windows with MinGW:
  - use MSVC (Visual Studio).
- TypeScript submit complains about `package.json`:
  - add `user/package.json` to `[submit].include`.
