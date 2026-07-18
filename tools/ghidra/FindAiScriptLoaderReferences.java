// Locates callers that reference the stable AI script-loader literals.
// Run through Ghidra headless analysis; the original PE is read only.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.ReferenceManager;

public class FindAiScriptLoaderReferences extends GhidraScript {
    private static final String[] NEEDLES = {".scr", "MISSIONS\\SCRIPTS\\"};

    @Override
    public void run() throws Exception {
        Memory memory = currentProgram.getMemory();
        ReferenceManager references = currentProgram.getReferenceManager();
        for (String needle : NEEDLES) {
            byte[] bytes = (needle + "\0").getBytes("US-ASCII");
            Address address = memory.findBytes(memory.getMinAddress(), memory.getMaxAddress(), bytes, null, true, monitor);
            println("===== " + needle + " =====");
            if (address == null) { println("missing"); continue; }
            println("literal=" + address);
            for (Reference reference : references.getReferencesTo(address)) {
                Function caller = currentProgram.getFunctionManager().getFunctionContaining(reference.getFromAddress());
                println("reference=" + reference.getFromAddress() + " caller=" + (caller == null ? "missing" : caller.getEntryPoint()));
            }
        }
    }
}
