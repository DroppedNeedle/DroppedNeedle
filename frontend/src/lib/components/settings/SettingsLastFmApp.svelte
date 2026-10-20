<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import {
		createSaveLastFmSettingsMutation,
		getLastFmSettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { Radio, ExternalLink } from 'lucide-svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	// The instance switch and the server's Last.fm app key pair. Users link
	// their own account and pick scrobble toggles in the profile's "Scrobbling
	// & Discovery" card. A user's own key pair, when saved, wins over this one
	// for that user. The secrets come back masked; saving the mask keeps the
	// stored value.
	const section = createSettingsSection(
		getLastFmSettingsQuery(),
		createSaveLastFmSettingsMutation()
	);
</script>

<Card variant="bordered">
	{#snippet header()}
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-lastfm/10 text-lastfm ring-1 ring-lastfm/20"
			>
				<Radio class="h-5 w-5" />
			</div>
			<div>
				<CardTitle>Last.fm</CardTitle>
				<p class="text-sm text-base-content/60">App credentials for the whole server.</p>
			</div>
		</div>
	{/snippet}
	<div class="mt-2 flex flex-col gap-4">
		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			Register one Last.fm API application and save its key and shared secret here; every user links
			with it. Each user then links <span class="font-medium">their own</span> Last.fm account and
			toggles scrobbling from their
			<a href={withBasePath('/profile')} class="link link-primary">profile</a>. Without a key here,
			Connect asks each user for their own key and secret; a user's own pair always wins over this
			one.
		</div>

		<QueryBoundary query={section} what="the Last.fm settings">
			{#snippet children(draft)}
				<div class="flex flex-col gap-4">
					<ToggleRow label="Allow Last.fm scrobbling and linking" bind:checked={draft.enabled} />

					<SecretField
						label="API key"
						mask={SECRET_MASK}
						placeholder="Last.fm API key"
						bind:value={draft.api_key}
					/>

					<SecretField
						label="Shared secret"
						mask={SECRET_MASK}
						placeholder="Shared secret"
						bind:value={draft.shared_secret}
					/>

					<a
						href="https://www.last.fm/api/account/create"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" /> Register an app to get a key and secret
					</a>

					<SaveBar {...section.bar} />
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
