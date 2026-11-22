import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const hub = 'T-00000000-0000-4000-8000-0000000000a1'

// load runs the plugin with an amp CLI whose searches answer from found, by query.
async function load(t, found, env = {}) {
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
	const amp = {
		logger: { log() {} },
		registerTool: (tool) => { tools[tool.name] = tool },
		threads: { get: (id) => ({ appendUserMessage: async ({ content }) => posted.push({ thread: id, content }) }) },
	}
	const plugin = await import(`./personal/cp-peers/index.ts?fixture=${randomUUID()}`)
	await plugin.default(amp)
	const ask = (input) => tools.ask_peer.execute(input, { thread: { id: 'T-asker' } })
	return { tools, posted, ask }
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
	assert.equal(await f.ask({ peer: 'ally', message: 'Up for a beer?' }), `Passed to alice's thread, https://ampcode.com/threads/${hub}, which sends it. When alice answers, this thread gets a link to the conversation that handles it.`)
	// The name as asked: the peer's thread resolves it with cpd.
	assert.deepEqual(f.posted, [{ thread: hub, content: 'My thread T-asker asks you to send ally this, with replyTo T-asker:\n\nUp for a beer?' }])
	assert.match(await f.ask({ peer: 'carol', message: 'hi' }), /carol has no thread in your clanker-proxy inbox yet/)
	assert.match(await f.ask({ peer: 'Not A Name', message: 'hi' }), /peer must be a peer name or alias/)
	assert.equal(f.posted.length, 1)
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
