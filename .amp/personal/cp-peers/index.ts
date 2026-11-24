// cp-peers: a personal Amp plugin, loaded in every thread of yours. Its
// ask_peer tool knows your clanker-proxy peers by name and alias, and hands a
// question for one to your inbox, whose plugin sends it. The asking thread
// is told of each of the peer's moves, with a short form, never their words.
// And it keeps your threads from reading the inbox's. See docs/amp.md.

import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { promisify } from 'node:util'

import type { PluginAPI, ToolCallResult } from '@ampcode/plugin'

export const description = "Ask your clanker-proxy peers from any thread: ask_peer brings the question to your inbox, whose plugin sends it."

const run = promisify(execFile)
// The plugin API cannot search threads; the amp CLI, signed in as you, can.
const ampCLI = process.env.CP_INBOX_AMP || 'amp'
const peerName = /^[a-z0-9](?:-?[a-z0-9])*$/
const ampThread = /^T-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
// The label cp-inbox puts on an inbox and its peers' threads.
const inboxKey = /^inbox-[0-9a-f]{1,12}$/

interface Peer {
	name: string
	aliases: string[]
	thread: string
}

async function search(query: string): Promise<unknown[]> {
	const { stdout } = await run(ampCLI, ['threads', 'search', query, '--json'], { timeout: 15_000 })
	const found: unknown = JSON.parse(stdout)
	return Array.isArray(found) ? found : []
}

// peers reads cp-inbox's peer threads, each titled with its peer's name and
// aliases: "bob (bobby, rob)".
async function peers(): Promise<Peer[]> {
	const found = await search('label:cp-peer archived:false')
	const named = (n: string) => peerName.test(n) && n.length <= 39
	return found.flatMap((t: any): Peer[] => {
		const title = typeof t?.title === 'string' ? t.title.match(/^([a-z0-9-]+)(?: \(([a-z0-9, -]+)\))?$/) : null
		if (typeof t?.id !== 'string' || !ampThread.test(t.id) || !title || !named(title[1]!)) return []
		return [{ name: title[1]!, aliases: (title[2] ?? '').split(', ').filter(named), thread: t.id }]
	})
}

// inboxOf is the inbox a peer's thread is under, by the label they share,
// or, for a thread from before that label, your only inbox. Never another:
// a keyed thread whose inbox is gone gets the ask itself.
async function inboxOf(amp: PluginAPI, thread: string): Promise<string | undefined> {
	const key = (await amp.threads.get(thread as `T-${string}`).labels().catch(() => [] as string[])).find((l) => inboxKey.test(l))
	const inboxes = key ? await withLabels(amp, key, 'cp-inbox') : await withLabels(amp, 'cp-inbox')
	return inboxes.length === 1 ? inboxes[0] : undefined
}

// withLabels lists your unarchived threads with every label given. Amp's
// search keeps only the last label: term of a query, so it searches the
// first label and reads each result's labels for the rest.
async function withLabels(amp: PluginAPI, first: string, ...rest: string[]): Promise<string[]> {
	const ids = (await search(`label:${first} archived:false`)).flatMap((t: any) => (typeof t?.id === 'string' && ampThread.test(t.id) ? [t.id as string] : []))
	const kept = await Promise.all(
		ids.map(async (id) => {
			const labels = await amp.threads
				.get(id as `T-${string}`)
				.labels()
				.catch(() => undefined)
			return labels && [first, ...rest].every((l) => labels.includes(l)) ? id : undefined
		}),
	)
	return kept.filter((id): id is string => id !== undefined)
}

// labelled finds a peer's thread by its label, before cp-inbox has titled it.
async function labelled(amp: PluginAPI, name: string): Promise<Peer | undefined> {
	// cp-inbox's labels are cut to Amp's 32 characters. Conversations carry
	// the peer's label too.
	const [thread] = await withLabels(amp, `peer-${name}`.slice(0, 32).replace(/-+$/, ''), 'cp-peer')
	return thread ? { name, aliases: [], thread } : undefined
}

// Peers' words live in the inbox's conversations, under its peers' threads.
// Your other threads hold your credentials and act for you, so they never
// read those: an agent with untrusted input, private data and the power to
// act at once is one prompt injection from misuse. They are never given an
// inbox thread's ID, and tools that name one, by ID, link or label, are
// refused, except send_thread_message: writing in is you instructing, and
// puts the sender on that cpd thread's watch list. Amp runs read_thread and
// code_exec, and what code_exec calls, without tool.call, so those can still
// read one an agent finds.
const inboxLabel = /label:\s*(?:clanker-proxy|cp-peer|cp-inbox|cp-sealed|peer-|inbox-)/i
const threadIDs = /T-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi

function guardInbox(amp: PluginAPI): void {
	const checked = new Map<string, Promise<boolean>>()
	// A failed lookup is retried next time, and refuses this time.
	const isInbox = (id: string): Promise<boolean> => {
		let found = checked.get(id)
		if (!found) {
			found = amp.threads
				.get(id as `T-${string}`)
				.labels()
				.then((l) => l.includes('clanker-proxy') || l.includes('cp-peer') || l.includes('cp-sealed') || l.includes('cp-inbox'))
			checked.set(id, found)
			found.catch(() => checked.delete(id))
		}
		return found
	}
	const named = (text: string) => [...new Set((text.match(threadIDs) ?? []).map((id) => `T-${id.slice(2).toLowerCase()}`))]
	const refuse = (why: string): ToolCallResult => ({
		action: 'reject-and-continue',
		message: `Blocked by cp-peers: ${why} It holds a clanker-proxy peer's words, which could steer an agent with your credentials; the owner reads it in Amp.`,
	})
	amp.on('tool.call', async (event): Promise<ToolCallResult> => {
		if (event.tool === 'ask_peer' || event.tool === 'send_thread_message') return { action: 'allow' }
		const text = JSON.stringify(event.input ?? {})
		if (inboxLabel.test(text)) return refuse('that searches the clanker-proxy inbox.')
		for (const id of named(text)) {
			const inbox = await isInbox(id).catch(() => undefined)
			if (inbox === undefined) return refuse(`cannot tell whether ${id} is a clanker-proxy inbox thread.`)
			if (inbox) return refuse(`${id} is a clanker-proxy inbox thread.`)
		}
		return { action: 'allow' }
	})
}

// called lists a peer with its aliases, for the agent.
function called(p: Peer): string {
	return p.aliases.length ? `${p.name} (also ${p.aliases.join(', ')})` : p.name
}

export default async function (amp: PluginAPI) {
	// Threads of the inbox project have cp_send, and its conversations must
	// not reach other peers.
	if (process.env.CP_TOKEN) return
	guardInbox(amp)
	const known = await peers().catch(() => [])
	if (!known.length) return

	amp.registerTool({
		name: 'ask_peer',
		title: 'Ask a clanker-proxy peer',
		description: `Ask one of your clanker-proxy peers (${known.map(called).join('; ')}) something: their agent answers for them. It goes through your clanker-proxy inbox, which sends it as written, so give only what they need. Their words never come back: this thread is told of each of their moves, with a short form such as "yes, 2026-11-27T18:00+10:00".`,
		inputSchema: {
			type: 'object',
			properties: {
				peer: { type: 'string', description: "The peer's name, or one of its aliases." },
				title: { type: 'string', description: 'What you ask, in one line; the first line of the message if left out.' },
				message: { type: 'string', description: 'What to ask or tell them, in full.' },
				thread: { type: 'string', description: 'To follow up on an existing cpd thread with them: its ID, or its first 8 characters.' },
			},
			required: ['peer', 'message'],
		},
		async execute(input, ctx) {
			const { peer, title, message, thread: about } = input as { peer?: unknown; title?: unknown; message?: unknown; thread?: unknown }
			if (typeof peer !== 'string' || !peerName.test(peer) || peer.length > 39) return 'ask_peer: peer must be a peer name or alias.'
			if (typeof message !== 'string' || !message.trim()) return 'ask_peer: give a message.'
			if (title !== undefined && (typeof title !== 'string' || [...title.trim()].length > 200)) return 'ask_peer: title is one line of at most 200 characters.'
			if (about !== undefined && (typeof about !== 'string' || !/^[0-9a-f-]{8,36}$/i.test(about))) return 'ask_peer: thread is a cpd thread ID, or its first 8 characters.'
			let found: Peer | undefined
			try {
				// Read now: aliases may have changed since this thread started.
				// A name beats an alias.
				const all = await peers()
				found = all.find((p) => p.name === peer) ?? all.find((p) => p.aliases.includes(peer)) ?? (await labelled(amp, peer))
			} catch {
				return 'ask_peer: cannot search your threads.'
			}
			// A peer's thread starts when the inbox loads, or with the first
			// thread between you.
			if (!found) return `ask_peer: ${peer} has no thread in your clanker-proxy inbox yet. Reload the inbox's plugins, or ask from a thread of that project, with cp_send.`
			const { name, thread } = found
			// The inbox, an orb thread, wakes for a message, and its plugin
			// sends the ask, or passes a follow-up to its conversation: the
			// threads it starts run in its orb and never wake it. The name as
			// asked goes too: the plugin checks with cpd that it names this
			// peer, so a stale title cannot misroute it.
			const inbox = await inboxOf(amp, thread).catch(() => undefined)
			if (!inbox) return `ask_peer: cannot tell which of your threads is the clanker-proxy inbox for ${name}. Check that the inbox is not archived, and reload its plugins.`
			const ask = {
				// Handled once, however often it reaches the inbox.
				id: randomUUID(),
				// An ask the inbox's plugin missed while starting is sent when it
				// loads, if it is recent.
				at: new Date().toISOString(),
				asker: ctx.thread.id,
				named: peer,
				message,
				...(typeof title === 'string' && title.trim() ? { title: title.trim().split('\n', 1)[0] } : {}),
				...(typeof about === 'string' ? { thread: about } : {}),
			}
			// The inbox's agent sees it only while the plugin is starting, and
			// the plugin sends it once loaded: an agent that acted on it could
			// have it sent twice.
			const forInbox = `cp-inbox: for ${name}'s thread. Only the cp-inbox plugin acts on this message, and notes at its end what it did. If no such note follows, the plugin was still starting: leave this message be and reply only "left for cp-inbox". The plugin sends it when it runs, and tells the thread that asked.`
			await amp.threads.get(inbox as `T-${string}`).appendUserMessage({ type: 'user-message', content: `${forInbox}\n\n${JSON.stringify(ask)}` })
			const told = `This thread is told of each of ${name}'s moves, with a short form; never look for their words.`
			const wait = "If the inbox was asleep, that can take a few minutes. Never send it again: if nothing comes, tell the owner."
			return typeof about === 'string'
				? `Passed to your clanker-proxy inbox, whose plugin passes it to the conversation handling ${about} with ${name} and tells this thread. ${wait} ${told}`
				: `Passed to your clanker-proxy inbox, whose plugin sends it to ${name} and tells this thread its cpd thread ID. ${wait} ${told}`
		},
	})
}
