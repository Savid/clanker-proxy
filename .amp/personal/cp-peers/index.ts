// cp-peers: a personal Amp plugin, loaded in every thread of yours. Its
// ask_peer tool knows your clanker-proxy peers by name and alias, and hands a
// question for one to that peer's thread in your inbox project, which sends
// it with cp_send. When the peer answers, the asking thread is told so, with
// a short form of the answer, never their words. And it keeps your threads
// out of the inbox's. See docs/amp.md.

import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

import type { PluginAPI, ToolCallResult } from '@ampcode/plugin'

export const description = "Ask your clanker-proxy peers from any thread: ask_peer hands the question to that peer's thread in your inbox project."

const run = promisify(execFile)
// The plugin API cannot search threads; the amp CLI, signed in as you, can.
const ampCLI = process.env.CP_INBOX_AMP || 'amp'
const peerName = /^[a-z0-9](?:-?[a-z0-9])*$/
const ampThread = /^T-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

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

// labelled finds a peer's thread by its label, before cp-inbox has titled it.
async function labelled(name: string): Promise<Peer | undefined> {
	// cp-inbox's labels are cut to Amp's 32 characters.
	const [t]: any[] = await search(`label:cp-peer archived:false label:${`peer-${name}`.slice(0, 32).replace(/-+$/, '')}`)
	return typeof t?.id === 'string' && ampThread.test(t.id) ? { name, aliases: [], thread: t.id } : undefined
}

// Peers' words live in the inbox's conversations, under its peers' threads.
// Your other threads hold your credentials and act for you, so they never
// read those: an agent with untrusted input, private data and the power to
// act at once is one prompt injection from misuse. They are never given an
// inbox thread's ID, and tools that name one, by ID, link or label, are
// refused. Amp runs read_thread and code_exec, and what code_exec calls,
// without tool.call, so those can still read one an agent finds.
const inboxLabel = /label:\s*(?:clanker-proxy|cp-peer|peer-)/i
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
				.then((l) => l.includes('clanker-proxy') || l.includes('cp-peer'))
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
		if (event.tool === 'ask_peer') return { action: 'allow' }
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
		description: `Ask one of your clanker-proxy peers (${known.map(called).join('; ')}) something: their agent answers for them. It goes through that peer's thread in your clanker-proxy inbox, which sends only what they need. Their words never come back: this thread is told when they answer, with a short form such as "yes, 2026-11-27T18:00+10:00".`,
		inputSchema: {
			type: 'object',
			properties: {
				peer: { type: 'string', description: "The peer's name, or one of its aliases." },
				message: { type: 'string', description: 'What to ask or tell them, in full.' },
			},
			required: ['peer', 'message'],
		},
		async execute(input, ctx) {
			const { peer, message } = input as { peer?: unknown; message?: unknown }
			if (typeof peer !== 'string' || !peerName.test(peer) || peer.length > 39) return 'ask_peer: peer must be a peer name or alias.'
			if (typeof message !== 'string' || !message.trim()) return 'ask_peer: give a message.'
			let found: Peer | undefined
			try {
				// Read now: aliases may have changed since this thread started.
				// A name beats an alias.
				const all = await peers()
				found = all.find((p) => p.name === peer) ?? all.find((p) => p.aliases.includes(peer)) ?? (await labelled(peer))
			} catch {
				return 'ask_peer: cannot search your threads.'
			}
			// A peer's thread starts with the first thread between you.
			if (!found) return `ask_peer: ${peer} has no thread in your clanker-proxy inbox yet. Ask them first from a thread of that project, with cp_send.`
			const { name, thread } = found
			// The name as asked: the peer's thread resolves it with cpd, and
			// refuses any peer but its own, so a stale title cannot misroute it.
			// Its agent knows the peer by name only, so an alias is explained.
			const alias = peer === name ? '' : ` ${peer} is my alias for ${name}: give cp_send "${peer}", and it checks that with cpd.`
			await amp.threads.get(thread).appendUserMessage({
				type: 'user-message',
				content: `My thread ${ctx.thread.id} asks you to send ${peer} this, with replyTo ${ctx.thread.id}.${alias}\n\n${message}`,
			})
			return `Passed to ${name}'s thread in your clanker-proxy inbox, which sends it. When ${name} answers, this thread is told, with a short form of the answer; never look for their words.`
		},
	})
}
