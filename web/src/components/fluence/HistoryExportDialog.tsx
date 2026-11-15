import { useState } from 'react';

import { toast } from '@/components/fluence/Toasts';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { getHistory } from '@/ipc/history';
import { describeError } from '@/lib/errors';
import {
  collectHistoryForExport,
  historyCsv,
  historyTxt,
  localStamp,
  saveTextFile,
} from '@/lib/historyExport';

interface HistoryExportDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function HistoryExportDialog({
  open,
  onOpenChange,
}: HistoryExportDialogProps) {
  const [exporting, setExporting] = useState(false);

  const exportHistory = async (format: 'txt' | 'csv' | 'json') => {
    if (exporting) return;
    setExporting(true);
    try {
      // Export everything on this device, independent of the visible filters.
      const all = await collectHistoryForExport((page) => getHistory(page, null));
      if (all.length === 0) {
        toast('No transcriptions to export', 'info');
        return;
      }
      const stamp = localStamp();
      const payload =
        format === 'json'
          ? {
              content: JSON.stringify(all, null, 2),
              filename: `fluence-history-${stamp}.json`,
              mime: 'application/json',
            }
          : format === 'csv'
            ? {
                content: historyCsv(all),
                filename: `fluence-history-${stamp}.csv`,
                mime: 'text/csv',
              }
            : {
                content: historyTxt(all),
                filename: `fluence-history-${stamp}.txt`,
                mime: 'text/plain',
              };
      const result = await saveTextFile(
        payload.content,
        payload.filename,
        payload.mime,
      );
      // User dismissed the native save dialog: nothing was written, so
      // keep the export dialog open for another try.
      if (result === 'cancelled') return;
      toast(
        `Exported ${all.length} transcription${all.length === 1 ? '' : 's'}`,
        'success',
      );
      onOpenChange(false);
    } catch (err) {
      toast('Could not export history. ' + describeError(err), 'error');
    } finally {
      setExporting(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !exporting) onOpenChange(false);
      }}
    >
      <DialogContent aria-label="Export history">
        <DialogHeader>
          <DialogTitle>Export history</DialogTitle>
          <DialogDescription>
            Export all transcriptions on this device, newest first. Search and
            filters do not apply.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button
            variant="ghost"
            onClick={() => onOpenChange(false)}
            disabled={exporting}
          >
            Cancel
          </Button>
          <Button
            variant="secondary"
            onClick={() => void exportHistory('txt')}
            disabled={exporting}
          >
            {exporting ? 'Exporting…' : 'Text (.txt)'}
          </Button>
          <Button
            variant="secondary"
            onClick={() => void exportHistory('csv')}
            disabled={exporting}
          >
            {exporting ? 'Exporting…' : 'CSV (.csv)'}
          </Button>
          <Button
            variant="secondary"
            onClick={() => void exportHistory('json')}
            disabled={exporting}
          >
            {exporting ? 'Exporting…' : 'JSON (.json)'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
