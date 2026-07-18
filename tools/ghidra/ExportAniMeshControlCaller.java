// Emits the function containing the observed AniMesh Control-loader sequence.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportAniMeshControlCaller extends GhidraScript {
    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(0x100032e7L);
        Function function = currentProgram.getFunctionManager().getFunctionContaining(address);
        println("===== AniMesh Control caller =====");
        if (function == null) { println("missing"); return; }
        println("entry=" + function.getEntryPoint());
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
