# Live collaboration

Several people can edit one project at the same time. The project's owner
hosts a session from the editor, and the others join it. Everyone sees the
others' scene edits within a fraction of a second, and file changes within
about a second.

## Host a session

1. Open the project.
2. Open the **Collaboration** area. Use the editor-type switcher in any
   area header.
3. Enter your name.
4. Under **Host**, choose how guests reach you:
   - **Direct**: guests connect to your address and port (default 7777).
     Use this on a LAN, or when your router forwards the port.
   - **Relay**: the session goes through a relay (`rusting relay`). Enter
     the relay address and its token. The panel then shows a room code to
     give your guests.
5. Press **Host**.
6. The session card shows a **Session key**. Press **Copy** and give the
   key to the people you invite, together with your address or room code.
   Each session gets a new key.

## Join a session

1. Open the **Collaboration** area. Save your work first: joining
   switches the editor to the host's project.
2. Enter your name.
3. Under **Join**, enter the host's address, or the relay address, token
   and room code. Enter the session key from the host.
4. Press **Join**.

The host sends the whole project to you, and the editor opens your copy.
The copy is kept in `~/.cache/rusting/collab/<address or code>` and is
replaced each time you join.

## How edits combine

- The host orders every edit. When two people change the same field in
  the same moment, the one the host receives last wins.
- Edits are per field. One person can move an object while another renames
  it, and both changes stay.
- Undo and Redo undo only your own edits. The others' edits stay.
- The host saves the shared scenes about a second after each edit.
- Each person opens any scene in the project. You see the others' edits to
  the scene you have open.
- In the Hierarchy, a colored dot marks each object another person has
  selected. Hover the dot to see who.
- In the Scene View, a frustum in the same color shows where each other
  person's editor camera is.
- Other files (code, assets, project settings) are sent whole when they
  change on disk.

## Running shared code

A guest's copy of the project holds code the host sent. A host's project
can hold code a guest sent. Building or running that code can do anything
your user account can do. The first time you press Play, Build or Check
with such code, the editor asks you to confirm. It asks once per session.
The editor marks such a folder with a `.rusting-untrusted` file. It keeps
asking in that folder, after the session and after a restart, until you
trust it.
Press **Trust and Run** only if you trust everyone in the session.

## Security

All session traffic is encrypted end to end with the session key (Noise
`NNpsk0`, ChaCha20-Poly1305). A relay or anyone else on the network sees
only ciphertext and cannot change it unnoticed. Without the key, nobody can
join or read the session: the host refuses a wrong key, and the guest sees
"The host refused the session key". Give the key only to people you
invite, over a channel you trust. Received file paths are checked: a peer
cannot write outside the project folder, into build output (`target`,
`build`), or into any file or folder whose name starts with `.`, such as
`.git` hooks or `.cargo` config, which could run code without the trust
prompt. `rust-toolchain` files, Windows short names (`GIT~1`) and paths
not in plain `folder/file` form are refused too. Those files are not
synced.

The trust prompt guards the editor only. Another tool that builds the
project, such as `cargo` in a terminal or rust-analyzer in your code
editor, runs `build.rs` and macros from shared code without asking.

## Limits

- A file deleted on one machine is deleted on the others, except a scene
  someone has open: it stays open, a warning shows in the Console, and the
  scene is sent back to the others.
- A new scene is sent whole once. After that, only its edits travel.
- Files larger than 12 MiB are not sent while editing. They are sent when a
  guest joins.
- Moves, renames and visibility changes apply to the open scene in place.
  Other remote edits reload it, which can cost a frame in a large scene.
