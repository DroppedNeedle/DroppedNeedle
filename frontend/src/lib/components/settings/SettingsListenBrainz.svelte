<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { AudioLines, ExternalLink } from 'lucide-svelte';
	import { getListenBrainzConnectionQuery } from '$lib/queries/listenbrainz/ListenBrainzQuery.svelte';
	import {
		createSaveListenBrainzMutation,
		createVerifyListenBrainzMutation
	} from '$lib/queries/listenbrainz/ListenBrainzMutations.svelte';
	import type {
		ListenBrainzConnection,
		VerifyConnectionResponse
	} from '$lib/queries/listenbrainz/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	const connQuery = getListenBrainzConnectionQuery();
	const saveConn = createSaveListenBrainzMutation();
	const verify = createVerifyListenBrainzMutation();

	let connDraft = $state<ListenBrainzConnection | null>(null);
	let connDirty = $state(false);
	let verdict = $state<VerifyConnectionResponse | null>(null);

	interface FormMessage {
		type: 'success' | 'error';
		text: string;
	}

	let connMessage = $state<FormMessage | null>(null);

	// Sync server data into the editable drafts until the admin touches them,
	// so a background refetch never clobbers half-typed input.
	$effect(() => {
		const data = connQuery.data;
		if (data && !connDirty) connDraft = { ...data };
	});

	function editConn<K extends keyof ListenBrainzConnection>(
		key: K,
		value: ListenBrainzConnection[K]
	) {
		if (!connDraft) return;
		connDraft[key] = value;
		connDirty = true;
	}

	function errorText(err: unknown, fallback: string): string {
		return err instanceof Error && err.message ? err.message : fallback;
	}

	async function saveConnection() {
		if (!connDraft) return;
		connMessage = null;
		verdict = null;
		try {
			const saved = await saveConn.mutateAsync(connDraft);
			connDraft = { ...saved };
			connDirty = false;
			connMessage = { type: 'success', text: 'ListenBrainz settings saved.' };
		} catch (err) {
			connMessage = { type: 'error', text: errorText(err, 'Could not save settings.') };
		}
	}

	async function testConnection() {
		if (!connDraft) return;
		connMessage = null;
		try {
			verdict = await verify.mutateAsync(connDraft);
		} catch (err) {
			verdict = { valid: false, message: errorText(err, 'Could not reach ListenBrainz.') };
		}
	}
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
		<div class="mt-2 flex flex-col gap-4">
			{#if connQuery.isPending}
				<div data-testid="listenbrainz-skeleton" class="flex animate-pulse flex-col gap-3 py-6">
					<div class="h-10 rounded-lg bg-base-300"></div>
					<div class="h-10 rounded-lg bg-base-300"></div>
					<div class="h-4 w-48 rounded bg-base-300"></div>
				</div>
			{:else if connQuery.isError || !connDraft}
				<div class="alert alert-warning">
					<span>Could not load ListenBrainz settings. Check the connection and try again.</span>
					<Button size="sm" onclick={() => connQuery.refetch()}>Retry</Button>
				</div>
			{:else}
				<ToggleRow
					label="Enable ListenBrainz"
					bind:checked={() => connDraft!.enabled, (v) => editConn('enabled', v)}
				/>

				<TextField
					label="Username"
					placeholder="Your ListenBrainz username"
					bind:value={() => connDraft!.username, (v) => editConn('username', v)}
				/>

				<SecretField
					label="User token"
					mask={SECRET_MASK}
					placeholder="User token"
					help="Once saved, the token shows as a placeholder. Saving with the placeholder keeps the stored token."
					bind:value={() => connDraft!.user_token, (v) => editConn('user_token', v)}
				/>

				<a
					href="https://listenbrainz.org/profile/"
					target="_blank"
					rel="noopener noreferrer"
					class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
				>
					<ExternalLink class="h-3 w-3" /> Find your user token on your ListenBrainz profile
				</a>

				{#if verdict}
					<div
						role="status"
						class="alert"
						class:alert-success={verdict.valid}
						class:alert-error={!verdict.valid}
					>
						<span>{verdict.message}</span>
					</div>
				{/if}

				{#if connMessage}
					<div
						class="alert"
						class:alert-success={connMessage.type === 'success'}
						class:alert-error={connMessage.type === 'error'}
					>
						<span>{connMessage.text}</span>
					</div>
				{/if}

				<div class="flex justify-end gap-2 pt-1">
					<Button
						variant="ghost"
						pill
						loading={verify.isPending}
						disabled={saveConn.isPending}
						onclick={() => void testConnection()}
					>
						Test connection
					</Button>
					<Button
						variant="primary"
						pill
						loading={saveConn.isPending}
						onclick={() => void saveConnection()}
					>
						Save ListenBrainz settings
					</Button>
				</div>
			{/if}
		</div>
	</Card>
</div>
