<script lang="ts">
	import { untrack } from 'svelte';
	import { CircleAlert, Globe } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import {
		createSavePublicAddressMutation,
		getPublicAddressQuery
	} from '$lib/queries/settings/PublicAddressQueries.svelte';
	import ListField from '$lib/ui/ListField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';

	const MAX_ADDRESSES = 8;

	const query = getPublicAddressQuery();
	const save = createSavePublicAddressMutation();

	let rows = $state<string[]>([]);
	// The saved list the rows were last filled from.
	let filledFrom: string[] = [];
	let status = $state<{ tone: 'success' | 'error'; message: string } | null>(null);
	let invalid = $state<string | null>(null);

	const saved = $derived(query.data?.origins ?? []);
	const entered = $derived(rows.map((row) => row.trim()).filter(Boolean));
	const dirty = $derived(!sameList(entered, saved));
	const pageAddress = window.location.origin;
	const pageListed = $derived(
		entered.some((address) => address.toLowerCase().replace(/\/$/, '') === pageAddress)
	);

	function sameList(a: string[], b: string[]): boolean {
		return a.length === b.length && a.every((value, index) => value === b[index]);
	}

	// Fill the rows from the saved list when it loads or changes on the
	// server, unless there are edits in progress.
	$effect(() => {
		const latest = saved;
		untrack(() => {
			if (sameList(entered, filledFrom)) rows = [...latest];
			filledFrom = latest;
		});
	});

	function addPageAddress() {
		rows = [...rows.filter((row) => row.trim()), pageAddress];
	}

	function discard() {
		rows = [...saved];
		status = null;
		invalid = null;
	}

	async function saveList() {
		status = null;
		invalid = null;
		try {
			const stored = await save.mutateAsync({ origins: entered });
			rows = [...stored.origins];
			filledFrom = stored.origins;
			status = { tone: 'success', message: 'Saved.' };
		} catch (error) {
			if (error instanceof ApiError && error.message === 'Origin not allowed') {
				status = {
					tone: 'error',
					message:
						"The server does not accept this page's address yet, so it refused the save. Open DroppedNeedle directly at its own address and port and save there, or run droppedneedle-tool public-address on the host."
				};
			} else if (error instanceof ApiError && error.status === 400) {
				invalid = error.message;
			} else {
				status = { tone: 'error', message: 'Could not save. Try again.' };
			}
		}
	}
</script>

<div class="card bg-base-200">
	<div class="card-body gap-4">
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

		<QueryBoundary {query} what="the public address">
			<ListField
				label="Addresses"
				itemLabel="Address"
				addLabel="Add an address"
				bind:values={rows}
				max={MAX_ADDRESSES}
				mono
				placeholder="https://music.example.com"
				empty="No address listed."
				help="The scheme, host and port, such as https://music.example.com or http://192.168.1.5:8688. Leave out any path. Up to {MAX_ADDRESSES}."
				error={invalid}
				disabled={save.isPending}
			>
				{#snippet actions()}
					<button
						type="button"
						class="btn btn-sm"
						disabled={save.isPending || pageListed || rows.length >= MAX_ADDRESSES}
						onclick={addPageAddress}
					>
						Use this page's address
					</button>
				{/snippet}
			</ListField>

			<SaveBar onsave={saveList} {dirty} saving={save.isPending} ondiscard={discard} {status} />
		</QueryBoundary>
	</div>
</div>
