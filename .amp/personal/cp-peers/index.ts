// cp-peers: a personal Amp plugin, loaded in every thread of yours. Its
// ask_peer tool hands a question for a clanker-proxy peer to that peer's
// thread in your inbox project, which sends it with cp_send. When the peer
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

// peerThreads returns cp-inbox's peer threads, which are named after their peer.
async function peerThreads(filter = ''): Promise<{ id: string; title: string }[]> {
	const { stdout } = await run(ampCLI, ['threads', 'search', `label:cp-peer archived:false ${filter}`.trim(), '--json'], { timeout: 15_000 })
	const found: unknown = JSON.parse(stdout)
	return Array.isArray(found) ? found.filter((t) => typeof t?.id === 'string' && ampThread.test(t.id)) : []
}

export default async function (amp: PluginAPI) {
	// Threads of the inbox project have cp_send, and its conversations must
	// not reach other peers.
	if (process.env.CP_TOKEN) return
	const peers = await peerThreads().then(
		(threads) => threads.map((t) => t.title).filter((t) => peerName.test(t)),
		() => [],
	)
	if (!peers.length) return

	amp.registerTool({
		name: 'ask_peer',
		title: 'Ask a clanker-proxy peer',
		description: `Ask one of your clanker-proxy peers (${peers.join(', ')}) something: their agent answers for them. It goes through that peer's thread in your clanker-proxy inbox, which sends only what they need. The answer does not come back as text: this thread gets a link to the conversation that handles it.`,
		inputSchema: {
			type: 'object',
			properties: {
				peer: { type: 'string', description: "The peer's name." },
				message: { type: 'string', description: 'What to ask or tell them, in full.' },
			},
			required: ['peer', 'message'],
		},
		async execute(input, ctx) {
			const { peer, message } = input as { peer?: unknown; message?: unknown }
			if (typeof peer !== 'string' || !peerName.test(peer) || peer.length > 39) return 'ask_peer: peer must be a peer name.'
			if (typeof message !== 'string' || !message.trim()) return 'ask_peer: give a message.'
			let hub: string | undefined
			try {
				// cp-inbox's labels are cut to Amp's 32 characters.
				hub = (await peerThreads(`label:${`peer-${peer}`.slice(0, 32).replace(/-+$/, '')}`))[0]?.id
			} catch {
				return 'ask_peer: cannot search your threads.'
			}
			// A peer's thread starts with the first thread between you.
			if (!hub) return `ask_peer: ${peer} has no thread in your clanker-proxy inbox yet. Ask them first from a thread of that project, with cp_send.`
			await amp.threads.get(hub).appendUserMessage({
				type: 'user-message',
				content: `My thread ${ctx.thread.id} asks you to send ${peer} this, with replyTo ${ctx.thread.id}:\n\n${message}`,
			})
			return `Passed to ${peer}'s thread, https://ampcode.com/threads/${hub}, which sends it. When ${peer} answers, this thread gets a link to the conversation that handles it.`
		},
	})
}
