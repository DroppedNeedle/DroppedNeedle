<script lang="ts">
	import { CircleAlert, Globe } from 'lucide-svelte';
	import {
		createSavePublicAddressMutation,
		getPublicAddressQuery,
		type PublicAddress
	} from '$lib/queries/settings/PublicAddressQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ListField from '$lib/ui/ListField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const MAX_ADDRESSES = 8;
	const pageAddress = window.location.origin;

	const section = createSettingsSection(
		getPublicAddressQuery(),
		createSavePublicAddressMutation(),
		{
			normalize: (draft: PublicAddress) => ({
				origins: draft.origins.map((row) => row.trim()).filter(Boolean)
			}),
			// Matched on the server's exact wording, which the README quotes
			// too. Reword the server's message and this advice is lost.
			explain: (error) =>
				error.message === 'Origin not allowed'
					? "The server does not accept this page's address yet, so it refused the save. Open DroppedNeedle directly at its own address and port and save there, or run droppedneedle-tool public-address on the host."
					: undefined
		}
	);

	function listsPage(origins: string[]): boolean {
		return origins.some(
			(address) => address.trim().toLowerCase().replace(/\/$/, '') === pageAddress
		);
	}

	function addPageAddress(draft: PublicAddress) {
		draft.origins = [...draft.origins.filter((row) => row.trim()), pageAddress];
	}
</script>

<Card>
	{#snippet header()}
		<div class="flex items-center gap-2">
			<div class="bg-secondary/10 rounded-lg p-2">
				<Globe class="h-5 w-5 text-secondary" />
			</div>
			<div>
				<h3 class="font-semibold">Public address</h3>
				<p class="text-xs text-base-content/50">
					Where people open DroppedNeedle, for when saving is refused there.
				</p>
			</div>
		</div>
	{/snippet}
	<div class="mt-2 flex flex-col gap-4">
		<p class="text-sm text-base-content/70">
			Behind some reverse proxies the server gets its own address wrong and answers every change
			with "Origin not allowed". List the address from the browser's address bar here and changes
			made from it are accepted. Most installs never need this.
		</p>

		<div class="alert alert-warning alert-soft text-sm">
			<CircleAlert class="h-4 w-4 shrink-0" />
			<span>
				Only list addresses at which this DroppedNeedle opens. A page at a listed address can make
				changes as whoever is signed in.
			</span>
		</div>

		<QueryBoundary query={section} what="the public address">
			{#snippet children(draft)}
				<ListField
					label="Addresses"
					itemLabel="Address"
					addLabel="Add an address"
					bind:values={draft.origins}
					max={MAX_ADDRESSES}
					mono
					placeholder="https://music.example.com"
					empty="No address listed."
					help="The scheme, host and port, such as https://music.example.com or http://192.168.1.5:8688. Leave out any path. Up to {MAX_ADDRESSES}."
					disabled={section.saving}
				>
					{#snippet actions()}
						<Button
							size="sm"
							disabled={section.saving ||
								listsPage(draft.origins) ||
								draft.origins.length >= MAX_ADDRESSES}
							onclick={() => addPageAddress(draft)}
						>
							Use this page's address
						</Button>
					{/snippet}
				</ListField>

				<SaveBar {...section.bar} />
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
