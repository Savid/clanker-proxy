import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const hub = 'T-00000000-0000-4000-8000-0000000000a1'

// load runs the plugin with an amp CLI whose searches answer from found, by query.
async function load(t, found, env = {}, labels = {}) {
	const dir = await mkdtemp(join(tmpdir(), 'cp-peers-test-'))
	const ampCLI = join(dir, 'amp')
	await writeFile(ampCLI, `#!/usr/bin/env node
const found = ${JSON.stringify(found)}
process.stdout.write(JSON.stringify(found[process.argv[4]] ?? []))
`, { mode: 0o700 })
	const vars = { CP_INBOX_AMP: ampCLI, CP_TOKEN: undefined, ...env }
	const previous = Object.fromEntries(Object.keys(vars).map((name) => [name, process.env[name]]))
	for (const [name, value] of Object.entries(vars)) {
		if (value === undefined) delete process.env[name]
		else process.env[name] = value
	}
	t.after(async () => {
		for (const [name, value] of Object.entries(previous)) {
			if (value === undefined) delete process.env[name]
			else process.env[name] = value
		}
		await rm(dir, { recursive: true, force: true })
	})
	const tools = {}
	const posted = []
	const hooks = {}
	const amp = {
		logger: { log() {} },
		registerTool: (tool) => { tools[tool.name] = tool },
		on: (name, fn) => { hooks[name] = fn },
		threads: {
			get: (id) => ({
				appendUserMessage: async ({ content }) => posted.push({ thread: id, content }),
				labels: async () => {
					if (!(id in labels)) throw new Error('not found')
					return labels[id]
				},
			}),
		},
	}
	const plugin = await import(`./personal/cp-peers/index.ts?fixture=${randomUUID()}`)
	await plugin.default(amp)
	const ask = (input) => tools.ask_peer.execute(input, { thread: { id: 'T-asker' } })
	const call = (tool, input) => hooks['tool.call']({ tool, input, thread: { id: 'T-asker' } })
	return { tools, posted, ask, hooks, call }
}

test("ask_peer knows your peers by name and alias, and hands the question to the peer's thread", async (t) => {
	const f = await load(t, {
		'label:cp-peer archived:false': [
			{ id: hub, title: 'alice (ally, al)' },
			{ id: 'T-bad', title: 'bob' },
			{ id: 'T-00000000-0000-4000-8000-0000000000a2', title: 'Clanker-proxy carol thread' },
		],
	})
	assert.match(f.tools.ask_peer.description, /clanker-proxy peers \(alice \(also ally, al\)\)/)
	assert.equal(
		await f.ask({ peer: 'ally', message: 'Up for a beer?' }),
		"Passed to alice's thread in your clanker-proxy inbox, which sends it. This thread is told of each of alice's moves, with a short form; never look for their words.",
	)
	// The name as asked, explained: the peer's thread resolves it with cpd.
	assert.deepEqual(f.posted, [
		{
			thread: hub,
			content: 'My thread T-asker asks you to send ally this, with replyTo T-asker. ally is my alias for alice: give cp_send "ally", and it checks that with cpd.\n\nUp for a beer?',
		},
	])
	await f.ask({ peer: 'alice', message: 'hi' })
	assert.equal(f.posted[1].content, 'My thread T-asker asks you to send alice this, with replyTo T-asker.\n\nhi')
	await f.ask({ peer: 'ally', message: 'Send him the command.', thread: '485eec32' })
	assert.equal(f.posted[2].content, 'My thread T-asker asks you to follow up on cpd thread 485eec32 with alice: pass this on with cp_follow_up, replyTo T-asker.\n\nSend him the command.')
	assert.match(await f.ask({ peer: 'alice', message: 'x', thread: 'nope' }), /thread is a cpd thread ID/)
	assert.match(await f.ask({ peer: 'carol', message: 'hi' }), /carol has no thread in your clanker-proxy inbox yet/)
	assert.match(await f.ask({ peer: 'Not A Name', message: 'hi' }), /peer must be a peer name or alias/)
	assert.equal(f.posted.length, 3)
})

test("ask_peer prefers a peer's name to another's alias, and finds an untitled peer thread by its label", async (t) => {
	const bob = 'T-00000000-0000-4000-8000-0000000000b0'
	const dave = 'T-00000000-0000-4000-8000-0000000000d0'
	const f = await load(t, {
		'label:cp-peer archived:false': [{ id: hub, title: 'alice (bob)' }, { id: bob, title: 'bob' }, { id: dave, title: 'Clanker-proxy dave thread' }],
		'label:cp-peer archived:false label:peer-dave': [{ id: dave, title: 'Clanker-proxy dave thread' }],
	})
	await f.ask({ peer: 'bob', message: 'hi' })
	await f.ask({ peer: 'dave', message: 'hi' })
	assert.deepEqual(f.posted.map((p) => p.thread), [bob, dave])
})

test('ask_peer stays out of the inbox project, and away without peers', async (t) => {
	const peers = { 'label:cp-peer archived:false': [{ id: hub, title: 'alice' }] }
	assert.equal((await load(t, peers, { CP_TOKEN: 'cpa_x' })).tools.ask_peer, undefined)
	assert.equal((await load(t, {})).tools.ask_peer, undefined)
})

test("your threads can write into the inbox's threads but not read them, as they hold peers' words", async (t) => {
	const conversation = 'T-00000000-0000-4000-8000-0000000000c1'
	const mine = 'T-00000000-0000-4000-8000-0000000000e1'
	const f = await load(t, {}, {}, { [conversation]: ['clanker-proxy', 'peer-alice'], [hub]: ['cp-peer'], [mine]: ['work'] })
	const allow = { action: 'allow' }
	assert.deepEqual(await f.call('read_thread', { threadID: mine, goal: 'x' }), allow)
	for (const [tool, input] of [
		['read_thread', { threadID: conversation }],
		['read_thread', { threadID: hub.toUpperCase().replace(/^T-/, 'T-') }],
		['shell_command', { cmd: `amp threads markdown https://ampcode.com/threads/${conversation}` }],
		['code_exec', { code: `await read_thread({ threadID: '${conversation}' })` }],
		['get_thread_status', { threadID: conversation }],
		['find_thread', { query: 'label:clanker-proxy beer' }],
		['read_thread', { threadID: 'T-00000000-0000-4000-8000-0000000000f9' }],
	]) {
		const v = await f.call(tool, input)
		assert.equal(v.action, 'reject-and-continue', `${tool} ${JSON.stringify(input)}`)
		assert.match(v.message, /^Blocked by cp-peers: /)
	}
	assert.deepEqual(await f.call('ask_peer', { peer: 'alice', message: `see ${conversation}` }), allow)
	// Writing in is the owner instructing.
	assert.deepEqual(await f.call('send_thread_message', { threadID: conversation, message: 'go with option (b)' }), allow)
	assert.deepEqual(await f.call('send_thread_message', { threadID: hub, message: 'hi' }), allow)
})
