<script lang="ts">
	import { CloudDownload } from 'lucide-svelte';

	import { getLidarrImportConfigQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import {
		saveLidarrConfigMutation,
		testLidarrMutation
	} from '$lib/queries/lidarr-import/LidarrImportMutations.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import LidarrImportSyncCard from './LidarrImportSyncCard.svelte';

	const test = testLidarrMutation();

	const section = createSettingsSection(getLidarrImportConfigQuery(), saveLidarrConfigMutation(), {
		test: async (draft) => {
			const result = await test.mutateAsync(draft);
			return { passed: result.valid, message: result.message };
		}
	});

	// Locked until a connection is saved. The masked config returns an empty
	// api_key when nothing is stored, so a non-empty pair means configured.
	const unlocked = $derived(
		(section.saved?.url ?? '').trim() !== '' && (section.saved?.api_key ?? '') !== ''
	);
</script>

<section class="space-y-4">
	<header class="space-y-1">
		<h2 class="text-lg font-semibold">Lidarr Import</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			Sync your monitored Lidarr artists into your follows, so you don't have to re-follow them by
			hand. Lidarr stays independent - this reads your monitored list, it doesn't manage anything.
		</p>
	</header>

	<Card variant="bordered" padding="none">
		<div>
			<div class="flex items-center gap-3 p-4">
				<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
					<CloudDownload class="size-6 text-accent" aria-hidden="true" />
				</div>
				<div>
					<h3 class="text-lg font-bold">Connection</h3>
					<p class="text-sm text-base-content/60">
						Save your Lidarr address and API key to unlock artist sync below.
					</p>
				</div>
			</div>

			<QueryBoundary query={section} what="the Lidarr connection">
				{#snippet children(draft)}
					<div class="space-y-4 border-t border-base-300 p-5">
						<TextField
							label="URL"
							mono
							placeholder="http://localhost:8686"
							help="Your Lidarr address. A plain http:// LAN URL is fine."
							bind:value={draft.url}
						/>

						<SecretField
							label="API key"
							mask={SECRET_MASK}
							placeholder="Lidarr → Settings → General → Security → API Key"
							bind:value={draft.api_key}
						/>

						<SaveBar {...section.bar} disabled={!draft.url.trim()} />
					</div>
				{/snippet}
			</QueryBoundary>
		</div>
	</Card>

	<LidarrImportSyncCard {unlocked} />
</section>
