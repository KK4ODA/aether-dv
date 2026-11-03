# Bench tests: IC-9700 and ID-52 PLUS on simplex

For Aether DV **0.2.1 or later**. Five tests, about 1½–2 hours in all.

Each test is recorded with **Diagnostics → Radio test** and ends with a summary you paste into the chat. Tests 1 and 2 matter most; if time is short, do those.

What the tests answer (IDs from `docs/10` §7):

| Test | Question | ID |
|---|---|---|
| 1 | Does a radio transmit data while it's receiving someone else? | HW-R3 |
| 2 | How quickly does the IC-9700 report voice over CI-V, and does Aether DV wait for it? | HW-R4 |
| 3 | Does CI-V read UR/R1/R2 correctly in VFO, memory and DR mode? | HW-R1 (first half) |
| 4 | Which D-STAR CI-V commands does the ID-52 PLUS answer? | HW-R2 |
| 5 | Real key-up delay and air time, against Aether DV's estimates | HW-R10 |

---

## Part A — Preparation (once)

### A1. What you need

- IC-9700 with its USB cable, and ID-52 PLUS with its USB-C cable.
- One computer (Windows assumed below) or two, with Aether DV 0.2.1 installed. Two computers are simpler (A5).
- For test 4 only: Bluetooth on the computer.
- An unused **simplex** frequency in a band both radios cover, not a calling frequency. Check your band plan for local DV simplex.
- **Lowest power on both radios.** Put them a few metres apart, or use dummy loads.

Both radios keep **your call sign as MY call**, so every transmission identifies you as usual.

### A2. Why two call signs inside Aether DV

Aether DV ignores frames carrying its own station call sign, so the two radios can't share one. Each radio gets its own engine and station call sign:

| Radio | Aether DV station call | Engine address |
|---|---|---|
| IC-9700 | `KK4ODA` | 127.0.0.1:7373 |
| ID-52 PLUS | `KK4ODA-1` | 127.0.0.1:7374 |

### A3. Folders and configuration files

Create two folders, so each engine keeps its own database, token and recordings:

```
C:\AetherTest\ic9700\
C:\AetherTest\id52\
```

**`C:\AetherTest\ic9700\ic9700.toml`**

```toml
callsign = "KK4ODA"
db = "aether.db"
api_listen = "127.0.0.1:7373"

[[port]]
kind = "serial"
name = "IC-9700"
path = "COM5"            # the IC-9700's DV data port, USB (B); see A4
baud = 9600
profile = "icom-ic9700"
mode = "slow"
flow = "none"

[port.civ]
path = "COM4"            # the IC-9700's CI-V port, USB (A); see A4
baud = 19200
addr = "A2"
allow_set = false
```

**`C:\AetherTest\id52\id52plus.toml`**

```toml
callsign = "KK4ODA-1"
db = "aether.db"
api_listen = "127.0.0.1:7374"

[[port]]
kind = "serial"
name = "ID-52PLUS"
path = "COM7"            # the ID-52 PLUS USB port; see A4
baud = 9600
profile = "icom-id52plus"
mode = "slow"
flow = "none"
```

Replace the COM numbers with yours (A4). Keep the files exactly in this form: one `[[port]]` each, and `[port.civ]` directly under the IC-9700's port.

### A4. Radio settings and finding the COM ports

**IC-9700** (menu names from Icom's manuals; your firmware may word them slightly differently):
- `SET > Connectors > USB (B)/DATA Function`: `USB (B) Function` = **DV Data**, `DV Data/GPS Out Baud Rate` = **9600**, `GPS Out` = **OFF**
- `SET > Connectors > CI-V`: `CI-V Address` = **A2h**, `CI-V Transceive` = **ON**, `CI-V USB Port` = **Unlink from [REMOTE]**, `CI-V USB Baud Rate` = **19200**, `CI-V USB Echo Back` = **OFF**
- `SET > DV/DD Set > DV Data TX` = **Auto**
- `SET > DV/DD Set > DV Fast Data > Fast Data` = **OFF**
- `MENU > GPS > GPS TX Mode` = **OFF** (GPS is its own icon on the MENU screen, not inside SET)
- Turn the radio off and on once after changing the USB settings.
- DV mode, **VFO**, your simplex frequency, **duplex OFF**, lowest power

The IC-9700 creates **two COM ports** in Windows Device Manager → Ports (both "Silicon Labs CP210x…"). One is USB (A), for CI-V; the other is USB (B), for DV data. You don't need to know which is which in advance: A6 shows whether you guessed right.

**ID-52 PLUS:**
- `MENU > SET > Function > USB Connect` = **Serialport**
- `MENU > SET > Function > USB Serialport Function` = **DV Data**
- `MENU > SET > DV Set > DV Data TX` = **Auto**
- DV Fast Data **OFF**, GPS TX **OFF**
- DV mode, same simplex frequency, duplex off, lowest power

Plug in the ID-52 PLUS. The new COM port in Device Manager → Ports is its data port.

### A5. Starting the two engines

**Two computers (simpler).** On each computer, open Aether DV. The connect screen shows **Configuration file**: enter the full path of that radio's `.toml` file, then click **Start the engine**. Everything below then happens in the app on each computer.

In 0.2.1 the connect screen sometimes shows only **Token file** and **Engine address**, without **Configuration file** and **Start the engine**. Close the app and open it again; if they're still missing, start the engine from PowerShell as for the ID-52 PLUS below (in `C:\AetherTest\ic9700`, with `ic9700.toml`), then enter `C:\AetherTest\ic9700\aetherdv.token` as **Token file** and click **Connect**.

**One computer.**
- **IC-9700, in the app:** open Aether DV, enter `C:\AetherTest\ic9700\ic9700.toml` as **Configuration file**, then **Start the engine**.
- **ID-52 PLUS, in a terminal:**
  1. Find the Aether DV program folder: right-click the Aether DV shortcut, then **Open file location**. If the shortcut opens a shortcut folder, repeat on the shortcut there. The folder holds `aetherdv.exe` and `aetherdv-cli.exe`.
  2. Open **PowerShell** and run (with your folder in place of `<AetherDV folder>`):

     ```powershell
     cd C:\AetherTest\id52
     & "<AetherDV folder>\aetherdv.exe" --config id52plus.toml
     ```

     Leave this window open; it's the ID-52 PLUS engine. **Ctrl+C** stops it.
  3. Open a **second PowerShell** window for commands to it. To save typing, define a short command once in that window:

     ```powershell
     function a52 { & "<AetherDV folder>\aetherdv-cli.exe" --api 127.0.0.1:7374 --token-file C:\AetherTest\id52\aetherdv.token @args }
     ```

     Then, for example, `a52 ports` lists its port, and `a52 send ID-52PLUS KK4ODA hello` sends a chat.

### A6. Check the connections

In the app (IC-9700), open **Diagnostics**:
- **"radio reports Fast Data OFF (slow data); matches the port configuration"** means the CI-V port is right.
- **"no answer from the radio at CI-V address A2h"** means you swapped the two COM ports. Swap `path` in `[port]` and `[port.civ]`, stop the engine (close and reopen Aether DV), and start again.
- The top bar shows **IC-9700 · Ready**. "Radio not found" means a wrong COM number or a port in use by another program.

For the ID-52 PLUS: `a52 ports` should show `"state":"ready"` (or check the app on the second computer).

### A7. First contact (do this before any test)

1. From the IC-9700 (app → **Chat**): To = `KK4ODA-1` (or *Everyone*), Send via = IC-9700, type "hello from 9700", press Enter.

   Up to 0.2.2, **To** is a list of stations already heard, so KK4ODA-1 isn't in it yet. Leave it on **Everyone (CQCQCQ)** for this first contact. After it, each side has heard the other and the call signs appear in the list. From 0.2.3 you can type any call sign in **To**; leave it empty for everyone.
2. The ID-52 PLUS keys up for a second or two. On the ID-52 side, `a52 history` (or the app's Chat on the second computer) shows the line.
3. Reply from the ID-52 PLUS: `a52 send ID-52PLUS KK4ODA hello from 52`. It appears in the IC-9700's Chat.

If either direction fails, don't run the tests. Write a support bundle (**Diagnostics → Write support bundle**) and tell me what happened.

---

## Part B — How each test is recorded

**In the app:** Diagnostics → **Radio test**:
1. Type the test name, for example "Test 1", then click **Start test**.
2. During the test, click a marker the moment something happens: **Keyed voice**, **Voice ended**, **Changed a radio setting**, **Something went wrong**. Or type a short note and press Enter. The exact time is what makes markers useful.
3. Click **Finish test**, then **Copy summary**, and paste the summary into the chat.

**From the terminal (ID-52 PLUS engine):**
- `a52 test start Test 1`
- `a52 test mark keyed voice`
- `a52 test finish` prints the summary; copy it from the window.

When a test involves both radios, start and finish the recording on **both** engines and paste both summaries.

---

## Part C — The tests

### Test 1: Does a radio transmit data while it's receiving? (HW-R3)

**Why it matters.** Aether DV only knows about voice on the channel if the radio reports it over CI-V. On radios without that, the only protection is the radio itself. If a radio *waits* while it receives, data never transmits over a voice user. If it *doesn't* wait, it does.

**Setup:** as in A7. The ID-52 PLUS engine has no CI-V, so it can't sense the voice: we're testing the radio, not Aether DV.

1. **ID-52 PLUS engine:** `a52 test start Test 1 ID-52 sends during voice`
2. **IC-9700:** hold PTT and talk (or count) for about **20 seconds**.
3. About 5 seconds in:
   - `a52 test mark keyed voice` (the IC-9700 is transmitting now)
   - `a52 send ID-52PLUS KK4ODA test one during voice`
4. **Watch the ID-52 PLUS's TX indicator** (red LED / TX on the display) during the next seconds. Then record what you saw, choosing the matching marker:
   - `a52 test mark ID-52 transmitted immediately during the voice`
   - `a52 test mark ID-52 waited, transmitted after the voice ended`
   - `a52 test mark ID-52 never transmitted`
5. Release the IC-9700's PTT, then `a52 test mark voice ended`.
6. Wait 30 seconds, then `a52 test finish`, and copy the summary.
7. Tell me whether the IC-9700 received "test one during voice" (app → Chat).

**Reverse direction (recommended):** test the IC-9700 as the sender.
1. Turn its voice sensing off for this run: in `ic9700.toml`, put `#` in front of the four `[port.civ]` lines.
2. Restart Aether DV and start the IC-9700 engine again.
3. Repeat the test with the ID-52 PLUS talking and the IC-9700 sending, using the Radio test card and markers in the app.
4. Remove the `#` again afterwards.

**What the results mean:**
- **Transmitted immediately:** radios without CI-V need the operator's **Pause sending** during voice. Aether DV's cautious timing for those radios stays as designed.
- **Waited:** the radio protects voice users by itself. We can relax the timing for radios without CI-V.

### Test 2: How fast does the IC-9700 report voice? (HW-R4)

**Setup:** `[port.civ]` active on the IC-9700 (default config).

1. **IC-9700 (app):** Radio test, "Test 2 voice sensing", **Start test**.
2. Check the IC-9700's plate in the top bar: it should say **Clear**.
3. **ID-52 PLUS:** key voice for about 5 seconds, release, wait about 10 seconds. Do this **three times**. Each time, on the IC-9700's card, click **Keyed voice** when the ID-52 PLUS keys and **Voice ended** when it releases.
4. During the **third** voice transmission, send a chat from the IC-9700: "test two". It should show **Queued**, and the plate should say **Voice on the channel**. It should go out only after the voice ends.
5. Wait 30 seconds, **Finish test**, **Copy summary**, and paste it.

**What the results mean:** the summary's timeline shows when the IC-9700 reported voice against your markers. A delay under about half a second is good. "Sending" while your marker said voice was on means a problem: tell me.

### Test 3: Does CI-V read UR/R1/R2 in every mode? (HW-R1, first half)

**No transmitting in this test.**

1. Add these lines **directly under the IC-9700 `[port.civ]` block** in `ic9700.toml`:

   ```toml
   [port.route]
   via = "repeater"
   repeater = "WX4GPB B"
   ```

   Restart Aether DV and start the engine. The IC-9700's plate turns amber. Sending is held, which is what we want here.
2. Radio test, "Test 3 call sign reads", **Start test**. Click the **IC-9700** plate to open the route panel; its "Radio" column shows what the radio reports for UR, R1 and R2.
3. Work through these. After each change, click **Changed a radio setting**, wait 5 seconds, compare the panel's "Radio" column with the radio's screen, and type a marker such as "VFO: matches" or "DR: R2 shows X but radio shows Y":
   1. **VFO mode:** set UR = `CQCQCQ`, R1 = `WX4GPB B`, R2 = blank.
   2. **VFO mode:** change R2 to `WX4GPB G`. The panel should switch to "Radio set for another route".
   3. **Memory mode:** select a memory channel that has D-STAR call signs stored.
   4. **DR mode:** select WX4GPB, choose **Local CQ**, then **Gateway CQ**.
   5. **Back to VFO**, and set R2 blank again.
4. **Finish test**, **Copy summary**, paste.
5. Delete the three `[port.route]` lines and restart.

If the panel doesn't change after a change on the radio, click **Check the radio again** in the panel. Tell me if that made the difference: it shows whether the IC-9700 announces changes by itself.

### Test 4: Which CI-V commands does the ID-52 PLUS answer? (HW-R2, optional)

1. On the ID-52 PLUS: turn Bluetooth on, and set the **Bluetooth** data device's Serialport Function to **CI-V**. USB stays **DV Data**.
2. Pair the radio in Windows Bluetooth settings. Windows creates "Standard Serial over Bluetooth link" COM ports; use the **Outgoing** one.
3. Check the radio's CI-V address (`MENU > SET > Function > CI-V`). It's reportedly **B4h**.
4. Add to `id52plus.toml`, under the port:

   ```toml
   [port.civ]
   path = "COM9"         # the Bluetooth Outgoing COM port
   baud = 19200
   addr = "B4"
   ```

5. Restart the ID-52 PLUS engine. Then:
   - `a52 test start Test 4 ID-52 CI-V`
   - wait **1 minute**
   - `a52 test finish`, and copy the summary.

**What the results mean:** the summary shows which reads answered:
- call signs (route lines);
- receive status (sources include `civ_status`);
- squelch (`civ_squelch`).

That decides whether the ID-52 PLUS gets voice sensing and route checking like the IC-9700.

### Test 5: Real timing (HW-R10)

1. Create a small test file on each computer, about 2 kB:

   ```powershell
   ("Aether DV test file line. " * 80) | Set-Content C:\AetherTest\test2k.txt
   ```

2. Start recordings on **both** engines: "Test 5 timing".
3. **IC-9700 → ID-52 PLUS:** three chats a few seconds apart, then the file:
   - app: Chat, To = KK4ODA-1, three short lines;
   - file, from a PowerShell window:

     ```powershell
     & "<AetherDV folder>\aetherdv-cli.exe" --api 127.0.0.1:7373 --token-file "$env:APPDATA\org.aetherdv.desktop\aetherdv.token" send-file IC-9700 KK4ODA-1 C:\AetherTest\test2k.txt
     ```

     That's the token file of the engine the app started. The app has no file-send button yet, so files always go through this command.
4. Wait until the file shows **Handed off** (app → Transfers).
5. **ID-52 PLUS → IC-9700:** three chats and the file:
   - `a52 send ID-52PLUS KK4ODA timing 1` (then 2, 3)
   - `a52 send-file ID-52PLUS KK4ODA C:\AetherTest\test2k.txt`
   - `a52 transfers` until done
6. **Finish test** on both engines and paste **both** summaries.

**What the results mean:** the IC-9700's summary lists every transmission with **key-up delay** and **real air time against the estimate**, measured from the radio's own transmit state. Those figures replace the placeholder timing values.

---

## Part D — If something goes wrong

| Symptom | Likely cause | Fix |
|---|---|---|
| "no answer from the radio at CI-V address A2h" | CI-V and data COM ports swapped, or the CI-V baud rate differs | Swap the two `path` values; set the radio's CI-V USB baud rate to 19200 |
| Port shows "Radio not found" | Wrong COM number, or another program has the port open (Icom software, another engine) | Check Device Manager; close other radio software |
| Chat shows **Queued** forever | Sending held: route (amber plate), paused, or the channel is busy | Click the plate to see why |
| Nothing received at all | Different frequency, Fast Data on at one end, DV Data TX not Auto | Check A4 on both radios |
| Received text is garbled | One radio has Fast Data on | Fast Data OFF on both |
| `a52` says it can't read the token | The engine isn't running, or it was started from another folder | Start it from `C:\AetherTest\id52` as in A5 |

Anything else: click **Something went wrong** on the test card, type what you saw, finish the test and paste the summary. If I ask for more, click **Send to the developer** on the finished test and follow the one line it shows.

## Part E — Checklist of what to send

- [ ] Test 1 summary (ID-52 PLUS), and whether the IC-9700 received "test one during voice"
- [ ] Test 1 reverse direction summary (IC-9700), if run
- [ ] Test 2 summary (IC-9700)
- [ ] Test 3 summary (IC-9700) and your per-mode notes
- [ ] Test 4 summary (ID-52 PLUS), if run
- [ ] Test 5 summaries (both)
