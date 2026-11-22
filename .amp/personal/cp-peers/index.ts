// cp-peers: a personal Amp plugin, loaded in every thread of yours. Its
// ask_peer tool knows your clanker-proxy peers by name and alias, and hands a
// question for one to that peer's thread in your inbox project, which sends
// it with cp_send. When the peer
// answers, the asking thread gets a link to the conversation that handles
// it, never their words. See docs/amp.md.

import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

import type { PluginAPI } from '@ampcode/plugin'

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

// peers reads cp-inbox's peer threads, each titled with its peer's name and
// aliases: "bob (bobby, rob)".
async function peers(): Promise<Peer[]> {
	const { stdout } = await run(ampCLI, ['threads', 'search', 'label:cp-peer archived:false', '--json'], { timeout: 15_000 })
	const found: unknown = JSON.parse(stdout)
	const named = (n: string) => peerName.test(n) && n.length <= 39
	return (Array.isArray(found) ? found : []).flatMap((t): Peer[] => {
		const title = typeof t?.title === 'string' ? t.title.match(/^([a-z0-9-]+)(?: \(([a-z0-9, -]+)\))?$/) : null
		if (typeof t?.id !== 'string' || !ampThread.test(t.id) || !title || !named(title[1]!)) return []
		return [{ name: title[1]!, aliases: (title[2] ?? '').split(', ').filter(named), thread: t.id }]
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
	const known = await peers().catch(() => [])
	if (!known.length) return

	amp.registerTool({
		name: 'ask_peer',
		title: 'Ask a clanker-proxy peer',
		description: `Ask one of your clanker-proxy peers (${known.map(called).join('; ')}) something: their agent answers for them. It goes through that peer's thread in your clanker-proxy inbox, which sends only what they need. The answer does not come back as text: this thread gets a link to the conversation that handles it.`,
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
				found = (await peers()).find((p) => p.name === peer || p.aliases.includes(peer))
			} catch {
				return 'ask_peer: cannot search your threads.'
			}
			// A peer's thread starts with the first thread between you.
			if (!found) return `ask_peer: ${peer} has no thread in your clanker-proxy inbox yet. Ask them first from a thread of that project, with cp_send.`
			const { name, thread } = found
			await amp.threads.get(thread).appendUserMessage({
				type: 'user-message',
				content: `My thread ${ctx.thread.id} asks you to send ${name} this, with replyTo ${ctx.thread.id}:\n\n${message}`,
			})
			return `Passed to ${name}'s thread, https://ampcode.com/threads/${thread}, which sends it. When ${name} answers, this thread gets a link to the conversation that handles it.`
		},
	})
}
