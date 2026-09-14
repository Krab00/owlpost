// owlpost as a Claude Code mod (function hooks, early access). Loaded next to the classic
// hooks in hooks.json, so a session without CLAUDE_CODE_ENABLE_FUNCTION_HOOKS keeps the
// plain plugin and a session with it gets the UI on top:
// - the band above the prompt shows the unseen inbox count (instead of the context line the
//   classic hook injects on every prompt and tool call, which this module strips);
// - a pane (`/owlpost:contacts`, `/owlpost:ask` without arguments, or the band's buttons)
//   browses and filters contacts, sends a question and walks the inbox, all by running `owl`
//   directly: no model turn, no tokens. Drafting an answer still goes through the model;
//   drafting a content request (OWL-039) does not — Draft there runs `owl draft` itself,
//   because that path reads one file and has no model in it.
//   Running the same command again (or the Close button) closes the pane.
// The inbox is a thread list (OWL-038): one row per person, Open draws the whole conversation
// with them oldest first, and the action buttons sit next to the open request only.
import type { EngineInterface, Register } from 'claude-code'

type $ = EngineInterface
type View = 'contacts' | 'ask' | 'threads' | 'thread'
type Contact = { name: string; emails: string[]; fingerprint: string; source: string; policy?: { mode: string } }
type Thread = { from: string; from_name: string; last_ts: string; unseen: number; open: number; last_summary: string }
type Ev = { ts: string; kind: string; dir: string; record_id: string; type: string; state: string; path?: string; text: string; by?: string; harness?: string }

const PANE = 'owlpost'
const MARK = '🦉 owlpost'
const POLL_MS = 15_000 // ponytail: polling `owl inbox --count`; the FileChanged wake below refreshes at once

let view: View = 'contacts'
let contacts: Contact[] = []
let threads: Thread[] = []
let timeline: Ev[] = []
let openPeer = '' // fingerprint of the conversation the `thread` view shows
let unseen = { count: 0, questions: 0 }
let filter = ''
let to: Contact | undefined
let path = ''
let question = ''
let reply: Record<string, string> = {}
let note = ''
let paneOpen = false

async function owl($: $, args: string[]): Promise<{ ok: boolean; out: string }> {
  try {
    const r = await $.process.run(['owl', ...args], { timeoutMs: 60_000 })
    return { ok: r.exitCode === 0, out: (r.exitCode === 0 ? r.stdout : r.stderr || r.stdout).trim() }
  } catch (err) {
    return { ok: false, out: String(err) }
  }
}

async function json<T>($: $, args: string[], fallback: T): Promise<T> {
  const r = await owl($, [...args, '--json'])
  if (!r.ok) return fallback
  try {
    return JSON.parse(r.out) as T
  } catch {
    return fallback
  }
}

async function refresh($: $) {
  ;[unseen, contacts, threads] = await Promise.all([
    json($, ['inbox', '--count'], { count: 0, questions: 0 }),
    json<Contact[]>($, ['contact', 'list'], []),
    // Exit 4 `no threads` is not an error here: the fallback is an empty list.
    json<Thread[]>($, ['thread'], []),
  ])
  contacts.sort((a, b) => a.name.localeCompare(b.name))
  if (openPeer) timeline = await json<Ev[]>($, ['thread', openPeer], [])
  $.ui.invalidate('ui.render')
}

// Opens one person's conversation, oldest first.
async function openThread($: $, fingerprint: string) {
  openPeer = fingerprint
  view = 'thread'
  timeline = await json<Ev[]>($, ['thread', fingerprint], [])
  $.ui.invalidate('ui.render')
}

// Runs one owl action, shows its output as the pane's note line, then reloads everything.
async function act($: $, args: string[]) {
  note = `… owl ${args.join(' ')}`
  $.ui.invalidate('ui.render')
  const r = await owl($, args)
  note = (r.ok ? '✓ ' : '✗ ') + (r.out.split('\n')[0] || `owl ${args[0]}`)
  await refresh($)
}

async function open($: $, next: View) {
  view = next
  paneOpen = true
  await $.ui.open({ id: PANE, title: 'owlpost', focus: true })
  await refresh($)
}

// Running the pane's command again while it shows that view closes it.
const toggle = ($: $, next: View) => (paneOpen && view === next ? $.ui.close({ id: PANE }) : open($, next))

// The classic hook's context line, minus ours: the band shows the count instead.
function strip<R extends { additionalContext?: string[] }>($: $, r: R): R {
  if (!r.additionalContext) return r
  void refresh($)
  return { ...r, additionalContext: r.additionalContext.filter((c) => !c.includes(MARK)) }
}

function matches(c: Contact, q: string) {
  const s = q.trim().toLowerCase()
  return !s || [c.name, c.fingerprint, ...c.emails].some((v) => v.toLowerCase().includes(s))
}

export const register: Register = (on) => {
  on('session.start', async ($, e, next) => {
    const r = await next(e)
    void refresh($)
    $.clock.every(POLL_MS, () => void refresh($))
    return r
  })

  // The band replaces the classic hook's per-prompt and per-tool context line.
  on('classic.UserPromptSubmit', async ($, e, next) => strip($, await next(e)))
  on('classic.PostToolUse', async ($, e, next) => strip($, await next(e)))
  on('classic.FileChanged', async ($, e, next) => {
    void refresh($)
    return next(e)
  })

  // Every close (Close button, the toggle, the person's close key) passes here.
  on('ui.close', { id: PANE }, async ($, e, next) => {
    paneOpen = false
    return next(e)
  })

  on('command.run', { command: 'owlpost:contacts' }, async ($) => {
    await toggle($, 'contacts')
    return {}
  })
  on('command.run', { command: 'owlpost:ask' }, async ($, e, next) => {
    if (e.args.trim()) return next(e)
    await toggle($, 'ask')
    return {}
  })

  on('ui.render', { component: 'AbovePrompt' }, ($, e, next) => {
    if (unseen.count === 0) return next(e)
    const { Box, Text, Button } = $.ui.resolve(e)
    const answers = unseen.count - unseen.questions
    const parts = [unseen.questions && `${unseen.questions} question(s)`, answers && `${answers} answer(s)`].filter(Boolean)
    return (
      <Box flexDirection="row" gap={2}>
        <Text color="yellow">{`${MARK}: ${parts.join(', ')}`}</Text>
        <Button label="Inbox" onPress={() => void open($, 'threads')} />
        <Button label="Contacts" onPress={() => void open($, 'contacts')} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, ($, e, next) => {
    if (e.surface === 'mobile') return next(e)
    const { Box, Text, Button, Input } = $.ui.resolve(e)
    const go = (v: View) => () => {
      view = v
      $.ui.invalidate('ui.render')
    }
    const policy = (c: Contact) => c.policy?.mode ?? '-'

    const contactsView = () => {
      const list = contacts.filter((c) => matches(c, filter))
      return (
        <Box flexDirection="column">
          <Input key="filter" label="Search" placeholder="name, e-mail or fingerprint" value={filter}
            onInput={(v) => { filter = v; $.ui.invalidate('ui.render') }} onSubmit={() => { if (list[0]) { to = list[0]; view = 'ask'; $.ui.invalidate('ui.render') } }} />
          {list.length === 0 && <Text dimColor>{contacts.length ? 'No match.' : 'No contacts yet — /owlpost:add a peer file.'}</Text>}
          {list.map((c) => (
            <Box key={c.fingerprint} flexDirection="row" gap={1}>
              <Box flexDirection="column" flexGrow={1}>
                <Text bold wrap="truncate">{c.name || c.fingerprint}</Text>
                <Text dimColor wrap="truncate">{`${c.emails.join(', ')} · ${c.fingerprint.slice(0, 12)} · ${c.source} · ${policy(c)}`}</Text>
              </Box>
              <Button label="Ask" onPress={() => { to = c; view = 'ask'; $.ui.invalidate('ui.render') }} />
              <Button label="Allow" onPress={() => void act($, ['allow', c.fingerprint])} />
              <Button label="Deny" onPress={() => void act($, ['deny', c.fingerprint])} />
            </Box>
          ))}
        </Box>
      )
    }

    const send = () => {
      if (!to || !question.trim()) {
        note = '✗ pick a contact and type a question'
        $.ui.invalidate('ui.render')
        return
      }
      const args = ['ask', '--peer', to.fingerprint, ...(path.trim() ? ['--file', path.trim()] : []), question.trim()]
      question = ''
      void act($, args)
    }

    const askView = () => (
      <Box flexDirection="column">
        {to ? (
          <Box flexDirection="row" gap={1}>
            <Text>{`To: ${to.name} <${to.emails[0] ?? to.fingerprint.slice(0, 12)}>`}</Text>
            <Button label="Change" onPress={() => { to = undefined; $.ui.invalidate('ui.render') }} />
          </Box>
        ) : (
          <Box flexDirection="column">
            <Input key="to" label="To" placeholder="start typing a name or e-mail" value={filter}
              onInput={(v) => { filter = v; $.ui.invalidate('ui.render') }}
              onSubmit={() => { to = contacts.find((c) => matches(c, filter)); $.ui.invalidate('ui.render') }} />
            {contacts.filter((c) => matches(c, filter)).slice(0, 6).map((c) => (
              <Button key={c.fingerprint} label={`${c.name} <${c.emails[0] ?? c.fingerprint.slice(0, 12)}>`}
                onPress={() => { to = c; $.ui.invalidate('ui.render') }} />
            ))}
          </Box>
        )}
        <Input key="path" label="Path (optional)" placeholder="src/lib.rs" value={path}
          onInput={(v) => { path = v }} onSubmit={(v) => { path = v }} />
        <Input key="question" label="Question" placeholder="what do you want to ask?" value={question} submitLabel="Send"
          onInput={(v) => { question = v }} onSubmit={(v) => { question = v; send() }} />
      </Box>
    )

    const threadsView = () => (
      <Box flexDirection="column">
        {threads.length === 0 && <Text dimColor>No conversations yet.</Text>}
        {threads.map((t) => (
          <Box key={t.from} flexDirection="row" gap={1} marginBottom={1}>
            <Box flexDirection="column" flexGrow={1}>
              <Text bold wrap="truncate" color={t.open > 0 ? 'yellow' : 'green'}>
                {`${t.from_name} · ${t.unseen} unseen · ${t.open} open`}
              </Text>
              <Text dimColor wrap="truncate">{`${t.from} · ${t.last_summary}`}</Text>
            </Box>
            <Button label="Open" onPress={() => void openThread($, t.from)} />
          </Box>
        ))}
      </Box>
    )

    // The open request of the conversation: the newest event of a question or content
    // record still waiting for the owner. Only that row carries the action buttons.
    const openRow = (() => {
      const wanted = ['consent', 'pending', 'drafted']
      for (let i = timeline.length - 1; i >= 0; i--) {
        const e = timeline[i]
        if (e.dir !== 'in' || (e.type !== 'question' && e.type !== 'content')) continue
        if (!wanted.includes(e.state)) continue
        // The newest event of that record, not an older one of the same record.
        const last = timeline.map((x) => x.record_id).lastIndexOf(e.record_id)
        if (last === i) return i
      }
      return -1
    })()

    // A content request (OWL-039) is a file the peer asked for, not a question: Draft runs
    // `owl draft` directly — there is no model in that path — and there is no own-answer
    // input, because the answer is the file.
    const actions = (e: Ev) => (
      <Box flexDirection="column">
        {e.type === 'content' && (
          <Text dimColor wrap="truncate">{`asks for ${e.text}`}</Text>
        )}
        <Box flexDirection="row" gap={1}>
          {e.state === 'consent' && <Button label={`Allow once (${openPeer})`} onPress={() => void act($, ['allow', openPeer, '--once'])} />}
          {e.state === 'consent' && <Button label={`Allow always (${openPeer})`} onPress={() => void act($, ['allow', openPeer, '--always'])} />}
          {e.state === 'consent' && <Button label={`Deny (${openPeer})`} onPress={() => void act($, ['deny', openPeer])} />}
          {e.type === 'content' && e.state !== 'consent' && e.state !== 'drafted' &&
            <Button label="Draft" onPress={() => void act($, ['draft', e.record_id])} />}
          {e.type !== 'content' && e.state !== 'consent' && e.state !== 'drafted' &&
            <Button label="Draft (Claude)" onPress={() => void $.command.run({ command: 'owlpost:draft', args: e.record_id })} />}
          {e.state === 'drafted' && <Button label="Send" onPress={() => void act($, ['send', e.record_id])} />}
          {e.state !== 'consent' && <Button label="Reject" onPress={() => void act($, ['reject', e.record_id])} />}
        </Box>
        {e.type !== 'content' && e.state !== 'consent' && (
          <Input key={`reply-${e.record_id}`} label="Own answer" placeholder="type and Enter to store it as the draft"
            value={reply[e.record_id] ?? ''} submitLabel="Save draft" onInput={(v) => { reply[e.record_id] = v }}
            onSubmit={(v) => { delete reply[e.record_id]; void act($, ['draft', e.record_id, '--text', v]) }} />
        )}
      </Box>
    )

    const threadView = () => {
      const peer = threads.find((t) => t.from === openPeer)
      return (
        <Box flexDirection="column">
          <Box flexDirection="row" gap={1}>
            <Button label="Back" onPress={() => { view = 'threads'; openPeer = ''; timeline = []; $.ui.invalidate('ui.render') }} />
            <Text bold wrap="truncate">{peer ? peer.from_name : openPeer}</Text>
          </Box>
          {timeline.length === 0 && <Text dimColor>Nothing in this conversation yet.</Text>}
          {timeline.map((e, i) => (
            <Box key={`${e.record_id}-${i}`} flexDirection="column" marginBottom={1}>
              <Text wrap="truncate" color={e.dir === 'in' ? 'yellow' : 'green'}>
                {`${e.ts.slice(11, 16)} ${e.kind}${e.by ? ` · by ${e.by}` : ''}${e.path && e.path !== '-' ? ` · ${e.path}` : ''}`}
              </Text>
              {e.text.split('\n').slice(0, 3).map((line, n) => (
                <Text key={`${e.record_id}-${i}-${n}`} wrap="truncate">{line}</Text>
              ))}
              {i === openRow && actions(e)}
            </Box>
          ))}
        </Box>
      )
    }

    return (
      <Box flexDirection="column">
        <Box flexDirection="row" gap={1}>
          <Button label={view === 'contacts' ? '[Contacts]' : 'Contacts'} onPress={go('contacts')} />
          <Button label={view === 'ask' ? '[Ask]' : 'Ask'} onPress={go('ask')} />
          <Button label={`${view === 'threads' || view === 'thread' ? '[Inbox' : 'Inbox'} ${threads.length}${view === 'threads' || view === 'thread' ? ']' : ''}`} onPress={go('threads')} />
          <Button label="Close" onPress={() => void $.ui.close({ id: PANE })} />
        </Box>
        {note && <Text dimColor wrap="truncate">{note}</Text>}
        {view === 'contacts' ? contactsView() : view === 'ask' ? askView() : view === 'thread' ? threadView() : threadsView()}
      </Box>
    )
  })
}
