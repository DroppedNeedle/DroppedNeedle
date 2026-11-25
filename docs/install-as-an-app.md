# Install DroppedNeedle as an app

The web UI can be installed on a phone, tablet or computer. It then opens from its own icon, in its own window, without the browser's address bar. It is the same web UI, signed in to the same server: nothing is downloaded from a store.

## Contents

- [Before you start](#before-you-start)
- [Android](#android)
- [iPhone and iPad](#iphone-and-ipad)
- [Computer](#computer)
- [What to expect](#what-to-expect)
- [Removing it](#removing-it)

## Before you start

- **Use an `https://` address with a real certificate**, such as the one your reverse proxy serves. On a plain `http://` address, such as `http://192.168.1.5:8688`, most browsers only add a shortcut that opens in the browser. See [Reverse proxy and security](reverse-proxy-and-security.md).
- **Behind a sign-in proxy** (Authelia, Authentik, Cloudflare Access and the like), let `/site.webmanifest` and the `android-chrome-*.png` icons through for signed-in visitors, after your base path if you set one. Without them the browser cannot read the app's name and icon, and offers no install.

## Android

**Chrome:**

1. Open your DroppedNeedle address and sign in.
2. Tap the menu (three dots, "More") at the right of the address bar.
3. Tap "Install and create shortcut", then "Install".

**Samsung Internet and Firefox** install it too, from their own menu: look for an install or "add to Home screen" option.

Once installed, sharing a song link from another app to DroppedNeedle (Share > DroppedNeedle) opens a search for that song. See [Song links](discovery.md#song-links).

## iPhone and iPad

1. Open your DroppedNeedle address in Safari.
2. Tap Share. Depending on your tabs layout, it is under the page menu button or in the toolbar.
3. Scroll and tap "Add to Home Screen". If it is missing, scroll to the bottom, tap "Edit Actions" and add it.
4. Keep "Open as Web App" on, then tap "Add".

The app keeps its own sign-in, apart from Safari's, so you may need to sign in once more the first time you open it. iPhone offers no Share > DroppedNeedle; a shortcut can do the same job, as [Song links](discovery.md#song-links) describes.

## Computer

- **Chrome:** click the install icon at the right of the address bar, or open the menu (three dots) and choose "Cast, save, and share", then "Install page as app".
- **Edge:** click the "App available" icon in the address bar, then "Install".
- **Safari on macOS 14 (Sonoma) or later:** choose File > "Add to Dock", then "Add".
- **Firefox on Windows,** version 143 or later, installs it too. Firefox on macOS and Linux does not.

For a desktop player with more features, Feishin connects to DroppedNeedle as a Subsonic server. See [Connect your music app](connect-your-app.md).

## What to expect

- **Lock screen and media keys:** play and pause work. The lock screen shows no song name or cover yet, and has no next or previous buttons.

- **Changing the base path:** if an admin changes `BASE_PATH`, the installed app no longer opens the server. Remove it and install it again from the new address.
- **No offline mode:** the app needs to reach your server, as the web UI does in a browser.

## Removing it

- **Android:** open Settings > Apps, tap DroppedNeedle, then "Uninstall".
- **iPhone and iPad:** touch and hold the icon on the Home Screen, then remove it.
- **Chrome on a computer:** open the app, open its menu (three dots), choose "Uninstall DroppedNeedle", then "Remove".
- **Edge:** open Settings and more > More tools > Apps > View apps, right-click DroppedNeedle and choose "Manage app", then uninstall it.
- **Safari on a Mac:** open the Applications folder in your home folder and move DroppedNeedle to the Bin.

Removing the app changes nothing on the server: your account, playlists and library stay as they are.
