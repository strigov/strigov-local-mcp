<div align="center">

[Русский](README.md) · **English**

# Strigov Local MCP

### Your folders, right in your ChatGPT conversation.

Read documents, find what you need, and ask about your files in regular Chat.<br>
You choose the folders. The app provides read-only access.

<img src="docs/assets/readme-cover.png" alt="Selected folders on a laptop connect to a conversation: a question becomes an answer with document sources" width="100%">

**[Download for macOS](https://github.com/strigov/strigov-local-mcp/releases/download/v2.2.1/Strigov-Local-MCP-2.2.1-macos-arm64.dmg)** · **[Download for Windows](https://github.com/strigov/strigov-local-mcp/releases/download/v2.2.1/Strigov-Local-MCP-2.2.1-windows-x64.zip)**

macOS Apple Silicon · Windows x64 · [MIT](LICENSE)

[Features](#features) · [Installation](#installation) · [Connect to ChatGPT](#connect-to-chatgpt) · [FAQ](#frequently-asked-questions)

</div>

## What is it for?

When you want to discuss a contract, find an email, or understand a spreadsheet, you can do it in **regular ChatGPT Chat, with its own message allowance**, without switching to Work or Codex agent mode just to read files.

Strigov Local MCP connects selected folders on your computer to ChatGPT. Set it up once, then ask about your documents without attaching them manually to every conversation. For example: “Find the Acme contract, show me the payment deadline, and include the page number.”

A conversation in Chat follows the selected chat model's limits. Work and Codex share their work usage limits; exact allowances and accounting depend on your plan and workspace. This app does not add an allowance or remove ChatGPT limits. [More about Work and Codex usage](https://learn.chatgpt.com/docs/pricing).

## Features

| | What you can do |
|---|---|
| 📁 **Choose your folders** | Connect several folders, give them friendly names, and remove access from the app menu. |
| 🔎 **Find what you need** | Search filenames and text inside documents. Read long documents in sections. |
| 📄 **Explore documents** | Read PDFs, Word documents, Excel files, saved emails, and attachments. Ask for source pages, sheets, or rows. |
| 🖼️ **View images and scans** | Open images and individual PDF pages, or zoom in on a detail. |
| 📦 **Look inside archives** | Browse ZIP files and read their contents without unpacking them manually. |
| 👁️ **Keep originals intact** | The app can read and search; it cannot change or delete your files. |

All these features are included in [version 2.2.1](https://github.com/strigov/strigov-local-mcp/releases/tag/v2.2.1), the current pre-release.

### Supported files

| Files | What ChatGPT receives |
|---|---|
| **PDF** | Text with page numbers; individual page views for scanned documents. |
| **Word: DOCX, DOCM** | Main document text, tables, comments, and tracked changes. |
| **Excel: XLSX, XLSM, XLS** | Saved cell values with sheet names and row numbers. Formulas are not recalculated. |
| **Email: EML, MSG** | Sender, subject, message text, and accessible attachments. |
| **ZIP** | File listings and supported documents inside, including nested archives. |
| **PNG, JPEG, GIF, WebP, TIFF, BMP** | An image or a selected region; TIFF supports page selection. |
| **Text, Markdown, CSV, source code** | UTF-8 or Windows-1251 text. |

Content search works with document text. There is no built-in optical character recognition (OCR): you can show a scanned page to ChatGPT, but folder searches cannot find words in an image without a text layer. Search inside ZIP files and email attachments separately by specifying the file inside.

<details>
<summary>If your file format is not supported</summary>

- Save HEIC/HEIF images as JPEG or PNG.
- Save DOC, PPT/PPTX, ODT/ODS, and RTF files in a suitable supported format, such as PDF.
- Extract RAR and 7z archives, or repack them as ZIP.
- Encrypted Office documents are not supported; there is no password entry for PDFs.
- Complex Word and Excel layouts are not fully preserved: Excel charts and Word headers and footers, for example, are not read. Save a PDF for visual inspection.

</details>

## Installation

### What you need

- **An Apple Silicon Mac (M1 or newer), running macOS 13+**, or **Windows 10/11 x64**. This release has no ready-made builds for Intel Macs or Windows ARM64.
- **A ChatGPT account that can add a custom MCP server using Tunnel**, plus access to tunnels in OpenAI Platform. MCP is simply how the app makes your files available to the chat; no programming is required.
- An internet connection and a computer that stays awake while you use its files.

Before installing, check that you can access [OpenAI Platform tunnels](https://platform.openai.com/settings/organization/tunnels) and add a custom connection in [ChatGPT Plugins](https://chatgpt.com/plugins). On a work account, an administrator may need to grant access. Installing this app does not grant those permissions.

You do not need to install developer tools or other supporting software. **The app's current interface is in Russian.** The instructions below include its exact labels with English translations.

### macOS

1. Open ** → About This Mac**. The “Chip” field should say Apple M1, M2, or a newer Apple chip.
2. [Download the Mac installer](https://github.com/strigov/strigov-local-mcp/releases/download/v2.2.1/Strigov-Local-MCP-2.2.1-macos-arm64.dmg). It is named `Strigov-Local-MCP-2.2.1-macos-arm64.dmg` and usually appears in Downloads.
3. Double-click the downloaded file. In the window that opens, launch **Install Strigov Local MCP.app**.
4. Click **«Установить» (Install)**. The app is installed in the `Applications` folder inside your home folder and starts automatically.
5. Look for the **server rack icon in the menu bar at the top of the screen**. The app does not open a large main window. Continue to [Connect to ChatGPT](#connect-to-chatgpt).

<details>
<summary><b>macOS will not open the installer</b></summary>

The current release is not notarized by Apple. If macOS says it cannot verify the developer, dismiss the message and open **System Settings → Privacy & Security → Open Anyway** for this installer.

If that does not help, you can remove the downloaded-file quarantine flag from this installer only:

1. Copy **Install Strigov Local MCP.app** from the open DMG into **Downloads**. The following steps use this copy, not the file inside the disk image.
2. Press **⌘ Space**, type **Terminal**, and open it.
3. Paste the entire command below and press **Enter**:

   ```sh
   xattr -dr com.apple.quarantine "$HOME/Downloads/Install Strigov Local MCP.app"
   ```

4. Open Downloads in Finder, double-click **Install Strigov Local MCP.app**, then click **«Установить» (Install)**.

This command expects that exact filename in Downloads. Use it only for the installer downloaded from this repository's link. It removes quarantine from that copy; you do not need to disable system-wide macOS protection. If you see `No such file`, check that you copied the app into Downloads without renaming it.

</details>

### Windows

1. [Download the Windows archive](https://github.com/strigov/strigov-local-mcp/releases/download/v2.2.1/Strigov-Local-MCP-2.2.1-windows-x64.zip). It is named `Strigov-Local-MCP-2.2.1-windows-x64.zip`.
2. Open Downloads, right-click the ZIP file, and choose **Extract All… → Extract**.
3. Open the extracted folder and run **`local-files-installer.exe`**. Keep the `payload` folder beside it: do not move the installer on its own or run it directly from the ZIP.
4. Click **«Установить» (Install)**. Administrator rights are not required.
5. Look for the **server rack icon near the clock**. If it is hidden, click the **˄** arrow to show hidden icons. Continue to [Connect to ChatGPT](#connect-to-chatgpt).

<details>
<summary><b>Windows says “Windows protected your PC”</b></summary>

The current release does not have a publisher signature. If you downloaded it from this repository's link, choose **More info → Run anyway**. Your organization's policies may restrict installation on a work computer; contact your administrator if so.

</details>

## Connect to ChatGPT

This is a one-time setup. A **tunnel** connects the app on your computer to OpenAI. You will need its **ID** (the connection's identifier) and a **key** (the secret used to access it).

### 1. Choose your folders

Click the app icon → **«Подключённые папки» (Connected folders) → «Добавить папку…» (Add folder)**. Choose a document folder, then repeat for any others. Use **«Изменить прозвище…» (Change nickname)** to give a folder a friendly name, such as “Contracts” or “Study”.

### 2. Create the connection in OpenAI

In the app menu, choose **«Подключение ChatGPT» (ChatGPT connection) → «Создать туннель в OpenAI Platform…» (Create a tunnel in OpenAI Platform)**. Create a tunnel in your organization and associate it with the ChatGPT workspace where you will use your files. Copy its **Tunnel ID**.

Next, choose **«Получить runtime API key…» (Get runtime API key)** in the same menu and create a key with **Tunnels Read + Use** permissions. You will enter it in the app in the next step; do not paste it into a ChatGPT conversation.

If Platform says **Tunnels access required**, ask your administrator for **Read + Manage** to create a tunnel and **Read + Use** to connect. [Official OpenAI instructions](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels).

### 3. Enter the details in the app

Choose **«Подключение ChatGPT» → «Настроить или сменить подключение…» (Set up or change connection)**. Paste the Tunnel ID into the first dialog and the key into the next. Review the displayed folders and confirm.

The app starts the connection. Wait for the menu to confirm it: the server rack icon should no longer be crossed out. Your key is saved in the computer's system credential store.

### 4. Add the connection in ChatGPT

Open [ChatGPT Plugins](https://chatgpt.com/plugins) in your browser:

1. Select **“+” → Add custom MCP server**.
2. Enter a name, such as **“My files”**.
3. Under **Connection**, choose **Tunnel**, then select your tunnel or paste its ID.
4. If an **Authentication** field is shown, choose **No authentication**: this app has no separate sign-in; access goes through your tunnel.
5. Review and accept the connection notice, then select **Create as a plugin**. Install the resulting plugin if prompted.

Labels may vary with the interface version. [OpenAI's connection guide](https://developers.openai.com/plugins/deploy/connect-chatgpt).

### 5. Ask your first question

Start a new conversation in **Chat** mode. Type **`@`**, select **“My files”** (or the name you chose), and send:

> Show me the folders you can access through “My files”.

Once you see your folders, try:

> Find payment deadlines in the “Contracts” folder. Include the filename and page number.

> Read the comments and tracked changes in “Agreement.docx”. What still needs to be agreed?

> Look at the “Expenses” sheet in “Budget.xlsx” and explain the largest items.

> Show me what is inside “Archive.zip” and summarize the document I choose.

> Open page three of “Scan.pdf” and help me read it.

## Access to your files

**You choose what the chat can access.** No folders are shared when you first launch the app. It provides no tools to edit files, delete them, or run commands. Some sensitive files, including `.env`, `.ssh`, and private keys, are also blocked.

**Original files stay on your computer; requested content is sent to OpenAI to answer your questions.** Processing is not entirely local. Only connect folders whose contents you are comfortable using in ChatGPT.

Manage access from the app menu:

- **«Отключить доступ» (Remove access)** in a folder's menu stops new requests to it. The files stay where they are.
- **«Остановить» (Stop)** stops the entire connection.
- **«Отключить аккаунт…» (Disconnect account)** stops the connection and removes the saved key. Your folder list is kept.

**«Закрыть значок» (Close tray icon) only closes the app's menu interface.** The background connection keeps running; use Stop to stop it.

## Frequently asked questions

<details>
<summary><b>Does my computer need to stay on?</b></summary>

Yes. Keep it powered on, connected to the internet, and awake, with the app's connection running. When the computer is off, ChatGPT cannot read new data from its folders.

</details>

<details>
<summary><b>ChatGPT cannot see my tunnel or folders</b></summary>

Check that the app menu confirms a connection, the folders have been added, and the tunnel is associated with the correct ChatGPT workspace. On macOS, allow folder access if the system asks. After adding a folder, ask ChatGPT to list the available folders again.

If you upgraded from 2.1.x and the new features are missing, open the connection in ChatGPT Plugins, select **Refresh**, and start a new conversation.

</details>

<details>
<summary><b>Why does viewing a PDF show “Allow file materialization?”</b></summary>

Some pages are delivered as individual one-page PDFs. Choose **Allow once** or **Always allow for this chat** so ChatGPT can open the page. The latter permission applies only to the current conversation.

</details>

<details>
<summary><b>How do I update the app?</b></summary>

Download the new installer from [Releases](https://github.com/strigov/strigov-local-mcp/releases) and install it the same way. Your folders and connection are preserved, and the previous app version is kept as a backup.

**On Windows, before updating**, choose **«Остановить» (Stop)**, then **«Закрыть значок» (Close tray icon)**. After installation, check the connection and choose **«Запустить» (Start)** if needed.

</details>

<details>
<summary><b>How do I open the app after restarting my computer?</b></summary>

On macOS, the menu bar icon starts when you sign in. On Windows, the connection starts in the background, but you need to open the tray icon separately: press **Win + R**, paste `%LOCALAPPDATA%\Programs\Strigov Local MCP`, and press Enter. Run `local-files.exe` from the folder that opens.

</details>

<details>
<summary><b>Can I connect another computer?</b></summary>

Yes. Install the app on the other computer and create a separate tunnel and ChatGPT connection for it. Each computer has its own selected folders.

</details>

## For developers

The [technical reference](docs/technical-reference.md) (in Russian) covers MCP tools, format limitations, security implementation, and build commands. Installer verification reports are attached to [release 2.2.1](https://github.com/strigov/strigov-local-mcp/releases/tag/v2.2.1).

## License

[MIT](LICENSE): you may use, modify, and distribute this project while preserving the license and copyright notice. Third-party components retain their own licenses.
