# Radio setup guides: how they are kept up to date

The app's **Radio Setup** tab shows `app/ui/radio-guides.json`, a tree of brand → model. Each model has setup steps, links and field reports. The file ships with the app, so corrections reach operators with the next update.

## Status of a radio

| Status | Meaning | When to use it |
|---|---|---|
| `documented` | From manuals and source research only | Default for every radio until someone reports back |
| `reported` | Operators report it working | At least one report says data worked (chat, a file or a form, in either direction) |
| `tested` | Tested by the project with Aether DV on this radio, with captures | Only with our own hardware-level test evidence (docs/07 HW tests) |

A model never moves to `tested` because of reports alone.

## Adding a report (instructions for Claude)

The owner pastes or forwards posts from groups.io, Facebook, forums or email.

1. **Read the whole post.** Identify the radio model and firmware, the connection (USB, Bluetooth, DATA cable), the menu settings used, what worked, what didn't, and whether it used Aether DV or D-RATS.
2. **Add an entry** to that model's `reports` array:
   ```json
   {
     "date": "2026-11-02",
     "source": "groups.io",
     "url": "https://groups.io/g/…/message/123",
     "reporter": "W4XYZ",
     "software": "Aether DV 0.1.0",
     "firmware": "1.50",
     "setup": "USB (B) = DV Data, 9600 baud, DV Data TX Auto",
     "worked": ["chat both ways", "10 KB file to D-RATS"],
     "problems": ["Fast Data on: nothing decoded"],
     "summary": "Slow data works through WX4GPB with an unlinked module."
   }
   ```
   - `date`: the date of the post.
   - `source`: one of groups.io, facebook, email, forum, qrz, reddit, in person, test, other.
   - `url`: only if the post is public.
   - `reporter`: a callsign only if the person signed the post with it; otherwise "name withheld". Facebook posts are not always public, so ask the owner before naming anyone from one.
3. **Steps and links.** If the report corrects or adds a menu step, update `steps` too, and keep the wording of the radio's own menus. Add a link only if it was given in the report or verified, and only https.
4. **Status.** Move `documented` → `reported` when a report says data actually worked. If reports contradict each other, keep both and say so in the summary.
5. **Never invent.** No guessed menu paths, firmware versions or links. Leave a field out rather than fill it in.
6. **Validate and record.** Set `updated` to today and run `python3 app/tools/check_guides.py` (CI runs it too). Commit with the source in the message, e.g. `Radio guides: ID-52 PLUS report from groups.io (W4XYZ)`.

## New models

Add the model under its brand with status `documented`. Fill in only what the manual or a report states. The `profile` field links the guide to the engine's radio profile (CI-V support, for example), so set it only when the engine knows that profile.
