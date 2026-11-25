<!--
	The support bundle: the admin reads it here, then Download saves exactly
	the text shown.
-->
<script lang="ts">
	import { Download, FileJson } from 'lucide-svelte';
	import { getSupportBundleQuery } from '$lib/features/settings/queries/diagnostics.svelte';
	import { saveBlob } from '$lib/utils/blobDownload';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	let open = $state(false);
	const query = getSupportBundleQuery(() => open);

	function save(text: string) {
		const day = new Date().toISOString().slice(0, 10);
		saveBlob(new Blob([text], { type: 'application/json' }), `droppedneedle-support-${day}.json`);
	}
</script>

<Button variant="outline" icon={FileJson} onclick={() => (open = true)}>
	Download support bundle
</Button>

<Modal bind:open title="Support bundle" size="lg" closeButton>
	<div class="space-y-3 text-sm">
		<p>
			<strong>In it:</strong> version, machine, free space by folder role, which settings are saved, background
			jobs, queue sizes, counts, enabled plugins, recent problems' codes and refs.
		</p>
		<p>
			<strong>Never in it:</strong> settings values, keys, paths, addresses, names, emails, what your
			library holds, message text.
		</p>
		<QueryBoundary {query} what="the support bundle">
			{#snippet children(data)}
				{@const text = JSON.stringify(data, null, 2)}
				<pre
					class="max-h-96 overflow-auto rounded border border-line p-3 font-mono text-xs"
					aria-label="Bundle JSON">{text}</pre>
				<div class="flex justify-end">
					<Button variant="primary" icon={Download} onclick={() => save(text)}>Download</Button>
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</Modal>
