<script lang="ts">
	import { AudioLines, ExternalLink } from 'lucide-svelte';
	import { getListenBrainzConnectionQuery } from '$lib/queries/listenbrainz/ListenBrainzQuery.svelte';
	import {
		createSaveListenBrainzMutation,
		createVerifyListenBrainzMutation
	} from '$lib/queries/listenbrainz/ListenBrainzMutations.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const verify = createVerifyListenBrainzMutation();

	const section = createSettingsSection(
		getListenBrainzConnectionQuery(),
		createSaveListenBrainzMutation(),
		{
			test: async (draft) => {
				const result = await verify.mutateAsync(draft);
				return { passed: result.valid, message: result.message };
			}
		}
	);
</script>

<div class="flex flex-col gap-6">
	<Card variant="bordered">
		{#snippet header()}
			<div class="flex items-center gap-3">
				<div
					class="flex h-11 w-11 items-center justify-center rounded-xl bg-listenbrainz/10 text-listenbrainz ring-1 ring-listenbrainz/20"
				>
					<AudioLines class="h-5 w-5" />
				</div>
				<div>
					<CardTitle>ListenBrainz</CardTitle>
					<p class="text-sm text-base-content/60">Instance connection for scrobbling.</p>
				</div>
			</div>
		{/snippet}
		<QueryBoundary query={section} what="the ListenBrainz settings">
			{#snippet children(draft)}
				<div class="mt-2 flex flex-col gap-4">
					<ToggleRow label="Enable ListenBrainz" bind:checked={draft.enabled} />

					<TextField
						label="Username"
						placeholder="Your ListenBrainz username"
						bind:value={draft.username}
					/>

					<SecretField
						label="User token"
						mask={SECRET_MASK}
						placeholder="User token"
						bind:value={draft.user_token}
					/>

					<a
						href="https://listenbrainz.org/profile/"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" /> Find your user token on your ListenBrainz profile
					</a>

					<SaveBar {...section.bar} testLabel="Test connection" />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</div>
